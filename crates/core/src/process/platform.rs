//! Platform boundary: handles and suspended Windows process association.
//! Windows: no external program instructions execute before Job Object assignment.
use std::{io, process::{Child, Command}};
#[cfg(windows)]
mod win {
    use super::*;
    use std::{ffi::c_void, mem::{size_of, zeroed}, os::windows::{io::AsRawHandle, process::CommandExt}, ptr::null_mut};
    type Handle = *mut c_void;
    const INVALID: Handle = -1isize as Handle;
    #[repr(C)]
    struct Basic { per_process:i64, per_job:i64, flags:u32, min:usize, max:usize, active:u32, affinity:usize, priority:u32, scheduling:u32 }
    #[repr(C)]
    struct IoCounters { reads:u64, writes:u64, other:u64, read_bytes:u64, write_bytes:u64, other_bytes:u64 }
    #[repr(C)]
    struct Extended { basic:Basic, io:IoCounters, process_limit:usize, job_limit:usize, peak_process:usize, peak_job:usize }
    #[repr(C)]
    struct ThreadEntry { size:u32, usage:u32, tid:u32, pid:u32, base:i32, delta:i32, flags:u32 }
    #[link(name="kernel32")]
    extern "system" {
        fn CreateJobObjectW(attrs: *mut c_void, name:*const u16)->Handle;
        fn SetInformationJobObject(job:Handle, class:i32, info:*const c_void, len:u32)->i32;
        fn AssignProcessToJobObject(job:Handle, process:Handle)->i32;
        fn TerminateJobObject(job:Handle, code:u32)->i32;
        fn CloseHandle(h:Handle)->i32;
        fn CreateToolhelp32Snapshot(flags:u32,pid:u32)->Handle;
        fn Thread32First(snapshot:Handle,entry:*mut ThreadEntry)->i32;
        fn Thread32Next(snapshot:Handle,entry:*mut ThreadEntry)->i32;
        fn OpenThread(access:u32,inherit:i32,tid:u32)->Handle;
        fn GetProcessIdOfThread(thread:Handle)->u32;
        fn ResumeThread(thread:Handle)->u32;
    }
    struct Owned(Handle);
    impl Drop for Owned { fn drop(&mut self) { unsafe { CloseHandle(self.0); } } }
    pub struct Tree(Owned);
    impl Tree {
        pub fn terminate(&self) { unsafe { TerminateJobObject(self.0.0,1); } }
    }
    fn last()->io::Error { io::Error::last_os_error() }
    fn resume(pid:u32)->io::Result<()> {
        // SAFETY: snapshot/thread handles are owned; entries are initialized with the ABI size.
        // CREATE_SUSPENDED guarantees the initial thread cannot execute user code until resumed.
        unsafe {
            let s=CreateToolhelp32Snapshot(4,0);
            if s==INVALID { return Err(last()); }
            let s=Owned(s);
            let mut e:ThreadEntry=zeroed(); e.size=size_of::<ThreadEntry>() as u32;
            if Thread32First(s.0,&mut e)==0 { return Err(last()); }
            let mut tid=None;
            loop {
                if e.pid==pid {
                    if tid.replace(e.tid).is_some() { return Err(io::Error::other("multiple threads in suspended process")); }
                }
                e.size=size_of::<ThreadEntry>() as u32;
                if Thread32Next(s.0,&mut e)==0 {
                    let error=last();
                    if error.raw_os_error()!=Some(18) { return Err(error); }
                    break;
                }
            }
            let t=OpenThread(0x0002|0x0800,0,tid.ok_or_else(||io::Error::other("initial thread not found"))?);
            if t.is_null() { return Err(last()); }
            let t=Owned(t);
            if GetProcessIdOfThread(t.0)!=pid { return Err(io::Error::other("initial thread owner mismatch")); }
            match ResumeThread(t.0) {
                1=>Ok(()), u32::MAX=>Err(last()), _=>Err(io::Error::other("unexpected suspend count")),
            }
        }
    }
    pub fn spawn(c:&mut Command)->io::Result<(Child,Tree)> {
        // SAFETY: all pointers refer to correctly sized initialized ABI structs or null;
        // Child owns its process handle, Owned owns every other Win32 handle.
        unsafe {
            let h=CreateJobObjectW(null_mut(),std::ptr::null());
            if h.is_null() { return Err(last()); }
            let tree=Tree(Owned(h));
            let mut info:Extended=zeroed();
            info.basic.flags=0x2000; // JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
            if SetInformationJobObject(h,9,(&info as *const Extended).cast(),size_of::<Extended>() as u32)==0 { return Err(last()); }
            // CREATE_SUSPENDED | CREATE_NO_WINDOW
            let mut child=c.creation_flags(0x00000004|0x08000000).spawn()?;
            let result=if AssignProcessToJobObject(h,child.as_raw_handle().cast())==0 { Err(last()) } else { resume(child.id()) };
            if let Err(e)=result {
                tree.terminate(); let _=child.kill(); let _=child.wait(); return Err(e);
            }
            Ok((child,tree))
        }
    }
}
#[cfg(windows)]
pub use win::{spawn,Tree};

#[cfg(unix)]
mod unix {
    use super::*;
    use std::os::unix::process::CommandExt;
    extern "C" { fn kill(pid:i32,sig:i32)->i32; }
    pub struct Tree(i32);
    impl Tree {
        pub fn terminate(&self) {
            // SAFETY: process_group(0) creates a distinct group whose ID is the child PID.
            // Negative PID targets that group, never the current process group.
            unsafe { kill(-self.0,9); }
        }
    }
    pub fn spawn(c:&mut Command)->io::Result<(Child,Tree)> {
        let child=c.process_group(0).spawn()?;
        let pid=child.id() as i32;
        Ok((child,Tree(pid)))
    }
}
#[cfg(unix)]
pub use unix::{spawn,Tree};
