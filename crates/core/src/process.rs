//! Bounded, shell-free subprocess execution. All calls are synchronous; callers use workers.
use std::{ffi::OsString, io::{self, Read}, path::{Path, PathBuf}, process::{Command, ExitStatus, Stdio}, sync::atomic::{AtomicBool, Ordering}, thread, time::{Duration, Instant}};
#[allow(unsafe_code)]
mod platform;

#[derive(Debug)]
pub struct Capture { pub bytes: Vec<u8>, pub discarded: u64 }
#[derive(Debug, PartialEq, Eq)]
pub enum Stop { Exited, Cancelled, TimedOut }
#[derive(Debug)]
pub struct Outcome { pub stop: Stop, pub status: ExitStatus, pub stdout: Capture, pub stderr: Capture }
#[derive(Debug, Clone)]
pub struct Request {
    pub exe: PathBuf, pub args: Vec<OsString>, pub timeout: Duration,
    pub stdout_limit: usize, pub stderr_limit: usize,
}
impl Request {
    pub fn new(exe: &Path) -> Self {
        Self { exe: exe.into(), args: vec![], timeout: Duration::from_secs(30),
               stdout_limit: 8 * 1024 * 1024, stderr_limit: 64 * 1024 }
    }
    pub fn args(mut self, args: impl IntoIterator<Item=impl Into<OsString>>) -> Self {
        self.args.extend(args.into_iter().map(Into::into)); self
    }
}
fn drain(mut r: impl Read, limit: usize) -> io::Result<Capture> {
    let mut bytes = Vec::new();
    let mut discarded = 0u64;
    let mut buf = [0u8; 8192];
    loop {
        let n = match r.read(&mut buf) { Ok(0) => break, Ok(n) => n, Err(e) if e.kind()==io::ErrorKind::Interrupted => continue, Err(e)=>return Err(e) };
        let keep = n.min(limit.saturating_sub(bytes.len()));
        bytes.extend_from_slice(&buf[..keep]);
        discarded = discarded.saturating_add((n-keep) as u64);
    }
    Ok(Capture { bytes, discarded })
}
pub fn run(req: &Request, cancelled: &AtomicBool) -> io::Result<Outcome> {
    let (outcome, stdout) = run_with_stdout(req, cancelled, |reader| drain(reader, req.stdout_limit))?;
    Ok(Outcome { stdout: stdout?, ..outcome })
}

/// Stream large stdout results through a caller-supplied reader while still
/// draining stderr and applying the normal cancellation and process-tree rules.
pub(crate) fn run_with_stdout<T: Send>(
    req: &Request,
    cancelled: &AtomicBool,
    read_stdout: impl FnOnce(std::process::ChildStdout) -> T + Send,
) -> io::Result<(Outcome, T)> {
    if !req.exe.is_absolute() { return Err(io::Error::new(io::ErrorKind::InvalidInput,crate::text::en(crate::text::Msg::AbsoluteExe))); }
    // Rust on Windows may implicitly invoke cmd.exe for .bat/.cmd. Permit PE .exe tools only.
    #[cfg(windows)]
    if !req.exe.extension().and_then(|e|e.to_str()).is_some_and(|e|e.eq_ignore_ascii_case("exe")) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput,crate::text::en(crate::text::Msg::ExeExtension)));
    }
    if cancelled.load(Ordering::Acquire) { return Err(io::Error::new(io::ErrorKind::Interrupted,crate::text::en(crate::text::Msg::CancelledLaunch))); }
    let mut command = Command::new(&req.exe);
    command.args(&req.args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let (mut child, tree) = platform::spawn(&mut command)?;
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let start = Instant::now();
    // Scoped readers are independent of caller/UI, and always drain both pipes.
    thread::scope(|scope| {
        let a = scope.spawn(move || read_stdout(stdout));
        let b = scope.spawn(move || drain(stderr, req.stderr_limit));
        let result = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Ok((Stop::Exited, status)),
                Err(e) => break Err(e),
                Ok(None) => {}
            }
            let stop = if cancelled.load(Ordering::Acquire) { Some(Stop::Cancelled) }
                       else if start.elapsed() >= req.timeout { Some(Stop::TimedOut) } else { None };
            if let Some(stop) = stop {
                tree.terminate();
                let _ = child.kill();
                break child.wait().map(|status| (stop,status));
            }
            thread::sleep(Duration::from_millis(15));
        };
        // Even a normally exiting parent may leave grandchildren holding pipes.
        tree.terminate();
        drop(tree);
        if result.is_err() { let _ = child.kill(); let _ = child.wait(); }
        let stdout = a.join().map_err(|_| io::Error::other(crate::text::en(crate::text::Msg::StdoutPanic)))?;
        let stderr = b.join().map_err(|_| io::Error::other(crate::text::en(crate::text::Msg::StderrPanic)))??;
        let (stop,status) = result?;
        Ok((Outcome { stop, status, stdout: Capture { bytes: Vec::new(), discarded: 0 }, stderr }, stdout))
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn bounded_capture() {
        let c=drain(&b"0123456789"[..],4).unwrap();
        assert_eq!(c.bytes,b"0123"); assert_eq!(c.discarded,6);
    }
    #[cfg(windows)]
    #[test] fn shell_scripts_rejected() {
        let p=std::env::current_exe().unwrap().with_extension("cmd");
        let err=run(&Request::new(&p), &AtomicBool::new(false)).unwrap_err();
        assert_eq!(err.kind(),io::ErrorKind::InvalidInput);
    }
    #[test] fn prelaunch_cancelled() {
        let p=std::env::current_exe().unwrap();
        assert_eq!(run(&Request::new(&p), &AtomicBool::new(true)).unwrap_err().kind(),io::ErrorKind::Interrupted);
    }
    #[test] fn relative_exe_rejected() {
        assert_eq!(run(&Request::new(Path::new("ffmpeg")), &AtomicBool::new(false)).unwrap_err().kind(),io::ErrorKind::InvalidInput);
    }
}
