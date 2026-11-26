//! Serial production queue shared by portable front ends.
use crate::{
    budget::Codec,
    dataset::Store,
    decision::Selection,
    hardware::Detector,
    pipeline::{self, EncodeRequest, Event},
};
use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Condvar, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub const CAPACITY: usize = 256;

#[derive(Clone, Copy, Debug)]
pub struct Settings {
    limit_bytes: u64,
    codec: Codec,
    max_long_edge: u32,
}

impl Settings {
    pub fn new(mib: u64, codec: Codec, max_long_edge: u32) -> Result<Self, &'static str> {
        let limit_bytes = mib
            .checked_mul(1_048_576)
            .filter(|&bytes| bytes != 0)
            .ok_or("Enter a positive whole number of MiB within range.")?;
        if max_long_edge < 2 {
            return Err("Invalid maximum resolution.");
        }
        Ok(Self {
            limit_bytes,
            codec,
            max_long_edge,
        })
    }

    pub fn limit_mib(self) -> u64 {
        self.limit_bytes / 1_048_576
    }

    pub fn codec(self) -> Codec {
        self.codec
    }

    pub fn max_long_edge(self) -> u32 {
        self.max_long_edge
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Waiting,
    Ready,
    Inspecting,
    Preflight,
    Encoding,
    Validating,
    Retrying,
    Cancelling,
    Published,
    Failed,
    Cancelled,
}

impl Phase {
    pub fn finished(self) -> bool {
        matches!(self, Self::Published | Self::Failed | Self::Cancelled)
    }
}

#[derive(Clone, Debug)]
pub struct Job {
    pub id: u32,
    pub source: PathBuf,
    pub settings: Settings,
    pub phase: Phase,
    pub encoder: Option<&'static str>,
    pub progress_percent: Option<u8>,
    pub estimated_bytes: Option<u64>,
    pub eta: Option<pipeline::eta::Estimate>,
    pub output: Option<PathBuf>,
    pub bytes: Option<u64>,
    pub elapsed: Option<Duration>,
    pub selection: Option<Selection>,
    pub detail: Vec<String>,
}

struct Entry {
    view: Arc<Job>,
    cancel: Arc<AtomicBool>,
}

#[derive(Default)]
struct State {
    entries: Vec<Entry>,
    next: u32,
    closing: bool,
}

#[derive(Default)]
struct Shared {
    state: Mutex<State>,
    ready: Condvar,
}

pub struct Queue {
    shared: Arc<Shared>,
    worker: JoinHandle<()>,
}

impl Queue {
    pub fn portable(root: PathBuf) -> io::Result<Self> {
        Self::spawn(move |shared| {
            let mut detector = Detector::default();
            work(shared, |job, cancel, update| {
                encode(&root, &mut detector, job, cancel, update)
            });
        })
    }

    fn spawn(
        run: impl FnOnce(&Shared) + Send + std::panic::UnwindSafe + 'static,
    ) -> io::Result<Self> {
        let shared = Arc::new(Shared::default());
        let background = Arc::clone(&shared);
        let worker = thread::Builder::new()
            .name("dotOrcA queue".into())
            .spawn(move || {
                if std::panic::catch_unwind(|| run(&background)).is_err() {
                    let mut state = background.state.lock().unwrap_or_else(|e| e.into_inner());
                    state.closing = true;
                    for entry in &mut state.entries {
                        if !entry.view.phase.finished() {
                            let job = Arc::make_mut(&mut entry.view);
                            job.phase = Phase::Failed;
                            job.detail.push("Queue worker stopped unexpectedly.".into());
                        }
                    }
                }
            })?;
        Ok(Self { shared, worker })
    }

    pub fn add(&self, source: &Path, settings: Settings) -> Result<u32, &'static str> {
        if !source.is_absolute() || source.file_stem().is_none() {
            return Err("Drop an absolute file path.");
        }
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.closing {
            return Err("Queue worker is unavailable.");
        }
        if state.entries.len() >= CAPACITY {
            return Err("Queue is full. Clear finished jobs to add more.");
        }
        state.next = state
            .next
            .checked_add(1)
            .ok_or("Queue ID range exhausted.")?;
        let id = state.next;
        state.entries.push(Entry {
            view: Arc::new(Job {
                id,
                source: source.into(),
                settings,
                phase: Phase::Waiting,
                encoder: None,
                progress_percent: None,
                estimated_bytes: None,
                eta: None,
                output: None,
                bytes: None,
                elapsed: None,
                selection: None,
                detail: Vec::new(),
            }),
            cancel: Arc::new(AtomicBool::new(false)),
        });
        Ok(id)
    }

    pub fn start(&self) -> Result<(), &'static str> {
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.closing {
            return Err("Queue worker is unavailable.");
        }
        for entry in &mut state.entries {
            if entry.view.phase == Phase::Waiting {
                Arc::make_mut(&mut entry.view).phase = Phase::Ready;
            }
        }
        self.shared.ready.notify_one();
        Ok(())
    }

    pub fn snapshot(&self) -> Vec<Arc<Job>> {
        let state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        state
            .entries
            .iter()
            .map(|entry| Arc::clone(&entry.view))
            .collect()
    }

    pub fn cancel(&self, id: u32) {
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        for entry in &mut state.entries {
            if entry.view.id == id || id == 0 {
                cancel_entry(entry);
            }
        }
    }

    pub fn clear_finished(&self) {
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        state.entries.retain(|entry| !entry.view.phase.finished());
    }

    /// Called after the window closes. Running subprocesses are cancelled and
    /// reaped and any publication already committed is allowed to finish.
    pub fn shutdown(self) {
        {
            let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
            state.closing = true;
            for entry in &mut state.entries {
                cancel_entry(entry);
            }
            self.shared.ready.notify_one();
        }
        let _ = self.worker.join();
    }
}

fn cancel_entry(entry: &mut Entry) {
    let phase = entry.view.phase;
    if phase.finished() || phase == Phase::Cancelling {
        return;
    }
    entry.cancel.store(true, Ordering::Release);
    let view = Arc::make_mut(&mut entry.view);
    if matches!(phase, Phase::Waiting | Phase::Ready) {
        view.phase = Phase::Cancelled;
        view.detail.push("Cancelled before processing.".into());
    } else {
        view.phase = Phase::Cancelling;
    }
}

struct Updates<'a> {
    shared: &'a Shared,
    id: u32,
}

impl Updates<'_> {
    fn modify<T>(&self, update: impl FnOnce(&mut Job) -> T) -> Option<T> {
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        state
            .entries
            .iter_mut()
            .find(|entry| entry.view.id == self.id)
            .map(|entry| update(Arc::make_mut(&mut entry.view)))
    }
}

fn work(shared: &Shared, mut run: impl FnMut(&Job, &AtomicBool, &Updates<'_>)) {
    loop {
        let (job, cancel) = {
            let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                if state.closing {
                    return;
                }
                if let Some(entry) = state
                    .entries
                    .iter_mut()
                    .find(|e| e.view.phase == Phase::Ready)
                {
                    Arc::make_mut(&mut entry.view).phase = Phase::Inspecting;
                    break (Arc::clone(&entry.view), Arc::clone(&entry.cancel));
                }
                state = shared.ready.wait(state).unwrap_or_else(|e| e.into_inner());
            }
        };
        run(&job, &cancel, &Updates { shared, id: job.id });
    }
}

fn note(job: &mut Job, detail: String) {
    let detail: String = detail
        .chars()
        .filter(|c| !c.is_control())
        .take(400)
        .collect();
    if job.detail.last() != Some(&detail) {
        if job.detail.len() == 24 {
            job.detail.remove(0);
        }
        job.detail.push(detail);
    }
}

fn observe(job: &mut Job, event: Event<'_>) {
    if job.phase == Phase::Cancelling {
        return;
    }
    match event {
        Event::Decision(decision) => {
            job.phase = Phase::Preflight;
            job.progress_percent = None;
            if job.selection != Some(decision.selection) {
                match decision.selection {
                    Selection::DefaultHardware(route) => {
                        job.encoder = Some(route.encoder(job.settings.codec));
                    }
                    Selection::SoftwareFallback(reason) => {
                        job.encoder =
                            Some(crate::decision::Route::Software.encoder(job.settings.codec));
                        note(job, format!("Software fallback: {reason:?}"));
                    }
                    Selection::ProactiveSoftware => {
                        job.encoder =
                            Some(crate::decision::Route::Software.encoder(job.settings.codec));
                    }
                    Selection::NoRoute => {
                        job.encoder = None;
                        for candidate in &decision.candidates {
                            note(
                                job,
                                format!("{}: {:?}", candidate.encoder, candidate.reasons),
                            );
                        }
                        note(
                            job,
                            format!("No compatible route. {:?}", decision.constraints),
                        );
                    }
                }
                job.selection = Some(decision.selection);
            }
        }
        Event::Preflight {
            route,
            result,
            detail,
            ..
        } => {
            job.encoder = Some(route.encoder(job.settings.codec));
            if result != crate::decision::CheckResult::Compatible {
                note(job, format!("{} preflight: {detail}", job.encoder.unwrap()));
            }
        }
        Event::Encoding { route, .. } => {
            job.phase = Phase::Encoding;
            job.encoder = Some(route.encoder(job.settings.codec));
            job.progress_percent = Some(0);
        }
        Event::Validating => {
            job.phase = Phase::Validating;
            job.progress_percent = None;
        }
        Event::RouteFailed { route, detail } => {
            note(
                job,
                format!("{} failed: {detail}", route.encoder(job.settings.codec)),
            );
        }
        Event::Oversize {
            measured,
            next_video_bps,
        } => {
            job.phase = Phase::Retrying;
            note(
                job,
                format!(
                    "Output exceeded limit ({measured} bytes). Retry bitrate: {next_video_bps:?}"
                ),
            );
        }
    }
}

fn encode(
    root: &Path,
    detector: &mut Detector,
    job: &Job,
    cancel: &AtomicBool,
    update: &Updates<'_>,
) {
    let result = (|| {
        if cancel.load(Ordering::Acquire) {
            return Err(pipeline::Error::Cancelled);
        }
        let dir = root.join("output");
        fs::create_dir_all(&dir).map_err(|e| {
            pipeline::Error::Io(io::Error::new(
                e.kind(),
                format!(
                    "Cannot create portable output folder {}: {e}",
                    dir.display()
                ),
            ))
        })?;
        let mut name = job
            .source
            .file_stem()
            .expect("checked on admission")
            .to_os_string();
        name.push(".mp4");
        let destination = dir.join(name);
        match fs::symlink_metadata(&destination) {
            Ok(_) => return Err(pipeline::Error::DestinationExists),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(pipeline::Error::Io(e)),
        }
        let tool = |name: &str| {
            root.join("tools")
                .join(format!("{name}{}", std::env::consts::EXE_SUFFIX))
        };
        let ffmpeg = tool("ffmpeg");
        let ffprobe = tool("ffprobe");
        for path in [&ffmpeg, &ffprobe] {
            if !path.is_file() {
                return Err(pipeline::Error::Io(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("Missing encoding tool {}", path.display()),
                )));
            }
        }
        let store = Store::portable(root).map_err(pipeline::Error::Io)?;
        pipeline::encode_and_learn_with_progress(
            &EncodeRequest {
                source: &job.source,
                destination: &destination,
                ffmpeg: &ffmpeg,
                ffprobe: &ffprobe,
                limit_bytes: job.settings.limit_bytes,
                codec: job.settings.codec,
                max_long_edge: job.settings.max_long_edge,
                timeout: Duration::from_secs(86_400),
            },
            detector,
            cancel,
            |event| {
                update.modify(|job| observe(job, event));
            },
            &store,
            Some(&|percent| {
                update.modify(|job| {
                    if job.phase == Phase::Encoding {
                        job.progress_percent = Some(percent);
                    }
                });
            }),
        )
    })();
    if let Some(mut completed) = update.modify(|current| current.clone()) {
        let job = &mut completed;
        job.progress_percent = None;
        match result {
            Ok(publication) => {
                let out = publication.output;
                job.phase = Phase::Published;
                job.output = Some(out.path);
                job.bytes = Some(out.bytes);
                job.elapsed = Some(out.elapsed);
                job.eta = out.production_eta;
                job.selection = Some(out.selection);
                job.encoder = Some(out.route.encoder(job.settings.codec));
                if let Err(error) = publication.learning {
                    note(
                        job,
                        format!("Output saved. Local learning write failed: {error}"),
                    );
                }
            }
            Err(pipeline::Error::Cancelled) => {
                job.phase = Phase::Cancelled;
                note(job, "Cancelled during processing.".into());
            }
            Err(error) => {
                job.phase = Phase::Failed;
                let detail = match error {
                    pipeline::Error::DestinationExists => {
                        "Output conflict: a file with this name already exists in output/.".into()
                    }
                    pipeline::Error::Io(error)
                        if error.kind() == io::ErrorKind::PermissionDenied =>
                    {
                        format!("Access denied. Check portable output write permission and input access: {error}")
                    }
                    pipeline::Error::Io(error) => error.to_string(),
                    other => format!("{other:?}"),
                };
                note(job, detail);
            }
        }
        if let Err(error) = write_log(root, job) {
            note(job, format!("Portable log write failed: {error}"));
        }
        update.modify(|job| *job = completed);
    }
}

fn write_log(root: &Path, job: &Job) -> io::Result<()> {
    let directory = root.join("logs");
    fs::create_dir_all(&directory)?;
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let path = directory.join(format!("{timestamp}-{}-{}.log", std::process::id(), job.id));
    let mut file = fs::File::options()
        .write(true)
        .create_new(true)
        .open(path)?;
    writeln!(file, "{job:#?}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        sync::{mpsc, Mutex},
        time::Instant,
    };

    fn settings() -> Settings {
        Settings::new(1, Codec::H264, 256).unwrap()
    }

    fn await_finished(queue: &Queue) -> Vec<Arc<Job>> {
        let deadline = Instant::now() + Duration::from_secs(90);
        loop {
            let jobs = queue.snapshot();
            if jobs.iter().all(|job| job.phase.finished()) {
                return jobs;
            }
            assert!(Instant::now() < deadline, "{jobs:?}");
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn active_worker_does_not_block_snapshot_add_or_cancel() {
        let (started, running) = mpsc::sync_channel(1);
        let (release, resume) = mpsc::sync_channel(1);
        let resume = Mutex::new(resume);
        let queue = Queue::spawn(move |shared| {
            work(shared, |_, cancel, update| {
                started.send(()).unwrap();
                resume
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(10))
                    .unwrap();
                assert!(cancel.load(Ordering::Acquire));
                update.modify(|job| job.phase = Phase::Cancelled);
            });
        })
        .unwrap();
        let source = std::env::temp_dir().join("小K 的鹅腿坟场 & (黑洞) ^ % !.avi");
        let active = queue.add(&source, settings()).unwrap();
        queue.start().unwrap();
        running.recv_timeout(Duration::from_secs(5)).unwrap();
        let snapshot = queue.snapshot();
        let waiting = queue.add(&source, settings()).unwrap();
        queue.cancel(waiting);
        queue.cancel(active);
        assert_eq!(snapshot[0].phase, Phase::Inspecting);
        assert_eq!(queue.snapshot()[0].phase, Phase::Cancelling);
        assert_eq!(queue.snapshot()[1].phase, Phase::Cancelled);
        release.send(()).unwrap();
        assert!(await_finished(&queue)
            .iter()
            .all(|j| j.phase == Phase::Cancelled));
        queue.clear_finished();
        assert!(queue.snapshot().is_empty());
        queue.shutdown();
    }

    #[test]
    #[ignore = "S06 CI supplies external FFmpeg and FFprobe"]
    fn production_special_path_batch_queue() {
        let root = std::env::temp_dir().join(format!("dotorca-queue-test-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(root.clone());
        let tools = root.join("tools");
        fs::create_dir(&tools).unwrap();
        let ffmpeg = tools.join(format!("ffmpeg{}", std::env::consts::EXE_SUFFIX));
        for (key, name) in [
            ("DOTORCA_TEST_FFMPEG", "ffmpeg"),
            ("DOTORCA_TEST_FFPROBE", "ffprobe"),
        ] {
            fs::copy(
                std::env::var_os(key).unwrap(),
                tools.join(format!("{name}{}", std::env::consts::EXE_SUFFIX)),
            )
            .unwrap();
        }
        let source = root.join("小K 的鹅腿坟场 & (黑洞) ^ % !.avi");
        let output = crate::process::run(
            &crate::process::Request::new(&ffmpeg)
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    "testsrc2=size=320x180:rate=25:duration=2",
                    "-c:v",
                    "ffv1",
                    "-f",
                    "avi",
                ])
                .args([source.as_os_str()]),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(output.status.success(), "{:?}", output.stderr);
        let queue = Queue::portable(root.clone()).unwrap();
        queue.add(&source, settings()).unwrap();
        queue.add(&source, settings()).unwrap();
        queue
            .add(&root.join("-missing 🎬.mp4"), settings())
            .unwrap();
        let cancelled = queue.add(&root.join("cancelled.mp4"), settings()).unwrap();
        queue.cancel(cancelled);
        queue.start().unwrap();

        let jobs = await_finished(&queue);
        assert_eq!(jobs[0].phase, Phase::Published, "{:?}", jobs[0]);
        let bytes = jobs[0].bytes.unwrap();
        assert!(bytes > 0 && bytes <= 1_048_576);
        assert_eq!(
            fs::metadata(jobs[0].output.as_ref().unwrap())
                .unwrap()
                .len(),
            bytes
        );
        assert!(jobs[0].eta.is_none());
        assert_eq!(jobs[1].phase, Phase::Failed);
        assert!(jobs[1].detail.last().unwrap().contains("Output conflict"));
        assert_eq!(jobs[2].phase, Phase::Failed);
        assert_eq!(jobs[3].phase, Phase::Cancelled);
        assert!(source.exists());
        assert!(
            root.join("data/local/production-v1.json").is_file(),
            "{:?}",
            jobs[0]
        );
        queue.shutdown();
    }
}
