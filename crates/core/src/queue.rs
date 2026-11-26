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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Bytes(u64),
    Percent(u8),
}

#[derive(Clone, Copy, Debug)]
pub struct Settings {
    target: Target,
    codec: Codec,
    max_long_edge: u32,
}

impl Settings {
    pub fn new(mib: u64, codec: Codec, max_long_edge: u32) -> Result<Self, &'static str> {
        let limit_bytes = mib
            .checked_mul(1_048_576)
            .filter(|&bytes| bytes != 0)
            .ok_or("Enter a positive whole number of MiB within range.")?;
        Self::with_target(Target::Bytes(limit_bytes), codec, max_long_edge)
    }

    pub fn percentage(percent: u64, codec: Codec, max_long_edge: u32) -> Result<Self, &'static str> {
        if !(1..=100).contains(&percent) {
            return Err("Enter a whole percentage from 1 to 100.");
        }
        Self::with_target(Target::Percent(percent as u8), codec, max_long_edge)
    }

    fn with_target(target: Target, codec: Codec, max_long_edge: u32) -> Result<Self, &'static str> {
        if max_long_edge < 2 {
            return Err("Invalid maximum resolution.");
        }
        Ok(Self { target, codec, max_long_edge })
    }

    pub fn target(self) -> Target {
        self.target
    }

    pub fn limit_bytes(self, source_bytes: u64) -> u64 {
        match self.target {
            Target::Bytes(bytes) => bytes,
            Target::Percent(percent) => ((u128::from(source_bytes) * u128::from(percent)) / 100) as u64,
        }
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
    Skipped,
    Failed,
    Cancelled,
}

impl Phase {
    pub fn finished(self) -> bool {
        matches!(self, Self::Published | Self::Skipped | Self::Failed | Self::Cancelled)
    }
}

#[derive(Clone, Debug)]
pub struct Job {
    pub id: u32,
    pub source: PathBuf,
    pub source_bytes: Option<u64>,
    pub source_size_pending: bool,
    pub settings: Option<Settings>,
    pub target_bytes: Option<u64>,
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
    pub encoder_error: Option<String>,
    pub retries: u32,
    pub source_info: Option<String>,
    pub target_info: Option<String>,
    pub output_info: Option<String>,
    pub failure: Option<String>,
    pub destination: Option<PathBuf>,
    pub skip_reason: Option<&'static str>,
}

impl Job {
    fn waiting(id: u32, source: &Path) -> Self {
        Self {
            id,
            source: source.into(),
            source_bytes: None,
            source_size_pending: true,
            settings: None,
            target_bytes: None,
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
            encoder_error: None,
            retries: 0,
            source_info: None,
            target_info: None,
            output_info: None,
            failure: None,
            destination: None,
            skip_reason: None,
        }
    }
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
    clearing: bool,
    active: Vec<(u32, String)>,
    parallelism: usize,
}

#[derive(Default)]
struct Shared {
    state: Mutex<State>,
    ready: Condvar,
    size_ready: Condvar,
}

pub struct Queue {
    shared: Arc<Shared>,
    worker: JoinHandle<()>,
}

impl Queue {
    pub fn portable(root: PathBuf) -> io::Result<Self> {
        Self::spawn(move |shared| {
            let parallelism = crate::hardware::queue_parallelism(&crate::hardware::inventory());
            {
                let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
                state.parallelism = parallelism;
            }
            thread::scope(|scope| {
                for _ in 0..parallelism {
                    let root = &root;
                    scope.spawn(move || {
                        let mut detector = Detector::default();
                        work(shared, |job, cancel, update| {
                            encode(root, &mut detector, job, cancel, update)
                        });
                    });
                }
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
                    background.size_ready.notify_one();
                    if state.clearing {
                        state.entries.clear();
                        state.clearing = false;
                    }
                    for entry in &mut state.entries {
                        if !entry.view.phase.finished() {
                            let job = Arc::make_mut(&mut entry.view);
                            job.phase = Phase::Failed;
                            job.detail.push("Queue worker stopped unexpectedly.".into());
                        }
                    }
                }
            })?;
        // A separate bounded metadata worker also services queued files while
        // encoding is active. It holds no lock during filesystem access.
        let sizes = Arc::clone(&shared);
        if let Err(error) = thread::Builder::new()
            .name("dotOrcA source sizes".into())
            .spawn(move || read_source_sizes(&sizes))
        {
            Self { shared, worker }.shutdown();
            return Err(error);
        }
        Ok(Self { shared, worker })
    }

    pub fn add(&self, source: &Path) -> Result<u32, &'static str> {
        if !source.is_absolute() || source.file_stem().is_none() {
            return Err("Drop an absolute file path.");
        }
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.closing {
            return Err("Queue worker is unavailable.");
        }
        if state.clearing {
            return Err("Wait for the active task to stop before adding files.");
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
            view: Arc::new(Job::waiting(id, source)),
            cancel: Arc::new(AtomicBool::new(false)),
        });
        self.shared.size_ready.notify_one();
        Ok(id)
    }

    pub fn start(&self, settings: Settings) -> Result<(), &'static str> {
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.closing {
            return Err("Queue worker is unavailable.");
        }
        if state.clearing {
            return Err("Wait for Clear All to finish.");
        }
        for entry in &mut state.entries {
            if entry.view.phase == Phase::Waiting {
                let job = Arc::make_mut(&mut entry.view);
                job.settings = Some(settings);
                job.phase = Phase::Ready;
            }
        }
        self.shared.ready.notify_all();
        Ok(())
    }

    pub fn snapshot(&self) -> Vec<Arc<Job>> {
        self.snapshot_with_clearing().0
    }

    pub fn snapshot_with_clearing(&self) -> (Vec<Arc<Job>>, bool) {
        let (jobs, clearing, _) = self.snapshot_status();
        (jobs, clearing)
    }

    pub fn snapshot_status(&self) -> (Vec<Arc<Job>>, bool, usize) {
        let state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        (state.entries.iter().map(|entry| Arc::clone(&entry.view)).collect(),
            state.clearing, state.parallelism.max(1))
    }

    pub fn cancel(&self, id: u32) {
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        for entry in &mut state.entries {
            if entry.view.id == id || id == 0 {
                cancel_entry(entry);
            }
        }
    }

    /// Reset only a terminal row. A fresh ID and cancel token prevent a
    /// delayed update from the previous attempt from contaminating the new one.
    pub fn reset(&self, id: u32) -> Result<(), &'static str> {
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.closing || state.clearing {
            return Err("Queue unavailable during shutdown or Clear All.");
        }
        let index = state.entries.iter().position(|e| e.view.id == id && e.view.phase.finished())
            .ok_or("Only finished tasks can be reset.")?;
        let next = state.next.checked_add(1).ok_or("Queue ID range exhausted.")?;
        let source = state.entries[index].view.source.clone();
        state.next = next;
        state.entries[index] = Entry {
            view: Arc::new(Job::waiting(next, &source)),
            cancel: Arc::new(AtomicBool::new(false)),
        };
        self.shared.size_ready.notify_one();
        Ok(())
    }

    pub fn clear_finished(&self) {
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        if !state.clearing {
            state.entries.retain(|entry| !entry.view.phase.finished());
        }
    }

    /// Stop all work without blocking the UI. The active worker clears the
    /// queue only after the encoder has exited and its cleanup has returned.
    pub fn clear_all(&self) {
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        for entry in &mut state.entries {
            cancel_entry(entry);
        }
        if state.active.is_empty() && state.entries.iter().all(|entry| entry.view.phase.finished()) {
            state.entries.clear();
            state.clearing = false;
        } else {
            state.clearing = true;
        }
    }

    pub fn is_clearing(&self) -> bool {
        self.shared.state.lock().unwrap_or_else(|e| e.into_inner()).clearing
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
            self.shared.ready.notify_all();
            self.shared.size_ready.notify_one();
        }
        // Do not join a metadata call that may be blocked by an offline UNC
        // share. The detached reader owns only Shared and exits on closing.
        let _ = self.worker.join();
    }
}

fn read_source_sizes(shared: &Shared) {
    loop {
        let (id, path) = {
            let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                if state.closing {
                    return;
                }
                if let Some(entry) = state.entries.iter().find(|e| e.view.source_size_pending) {
                    break (entry.view.id, entry.view.source.clone());
                }
                state = shared.size_ready.wait(state).unwrap_or_else(|e| e.into_inner());
            }
        };
        let bytes = fs::metadata(path).ok().filter(|m| m.is_file()).map(|m| m.len());
        Updates { shared, id }.modify(|job| {
            if job.source_size_pending {
                job.source_bytes = bytes;
                job.source_size_pending = false;
            }
        });
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

fn output_key(source: &Path) -> String {
    source.file_stem().unwrap_or_default().to_string_lossy().to_lowercase()
}

fn work(shared: &Shared, mut run: impl FnMut(&Job, &AtomicBool, &Updates<'_>)) {
    loop {
        let (job, cancel) = {
            let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                if state.closing {
                    return;
                }
                if let Some(index) = state.entries.iter().position(|e| {
                    e.view.phase == Phase::Ready && !state.active.iter()
                        .any(|(_, name)| *name == output_key(&e.view.source))
                }) {
                    let entry = &mut state.entries[index];
                    Arc::make_mut(&mut entry.view).phase = Phase::Inspecting;
                    let pair = (Arc::clone(&entry.view), Arc::clone(&entry.cancel));
                    state.active.push((pair.0.id, output_key(&pair.0.source)));
                    break pair;
                }
                state = shared.ready.wait(state).unwrap_or_else(|e| e.into_inner());
            }
        };
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run(&job, &cancel, &Updates { shared, id: job.id });
        }));
        let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
        state.active.retain(|(id, _)| *id != job.id);
        if result.is_err() {
            state.closing = true;
            for entry in &mut state.entries {
                if entry.view.id == job.id {
                    let view = Arc::make_mut(&mut entry.view);
                    view.phase = Phase::Failed;
                    view.failure = Some("Queue worker stopped unexpectedly.".into());
                } else {
                    cancel_entry(entry);
                }
            }
            shared.size_ready.notify_one();
        }
        if state.clearing && state.active.is_empty() {
            state.entries.clear();
            state.clearing = false;
        }
        shared.ready.notify_all();
        if result.is_err() { return; }
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
        Event::Source(source) => job.source_info = Some(media_summary(source)),
        Event::Plan(plan) => {
            let interlaced = matches!(plan.source.raw["field_order"].as_str(), Some("tt" | "bb" | "tb" | "bt"));
            job.target_info = Some(format!(
                "{:?} | {} × {} | {} | {:?} | {} | Video {:.0} kb/s | Audio tracks {}",
                plan.codec, plan.width, plan.height,
                plan.source.bit_depth.map_or_else(|| "unknown depth".into(), |n| format!("{n}-bit")),
                plan.source.hdr, if interlaced { "BWDIF 2× cadence" } else { "Original cadence" },
                plan.video_bps as f64 / 1000.0, plan.audio_bps.len()
            ));
        }
        Event::Decision(decision) => {
            job.phase = Phase::Preflight;
            job.progress_percent = None;
            if job.selection != Some(decision.selection) {
                match decision.selection {
                    Selection::DefaultHardware(route) => {
                        job.encoder = Some(route.encoder(job.settings.expect("started job").codec));
                    }
                    Selection::SoftwareFallback(reason) => {
                        job.encoder =
                            Some(crate::decision::Route::Software.encoder(job.settings.expect("started job").codec));
                        note(job, format!("Software fallback: {reason:?}"));
                    }
                    Selection::ProactiveSoftware => {
                        job.encoder =
                            Some(crate::decision::Route::Software.encoder(job.settings.expect("started job").codec));
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
            job.encoder = Some(route.encoder(job.settings.expect("started job").codec));
            if result != crate::decision::CheckResult::Compatible {
                note(job, format!("{} preflight: {detail}", job.encoder.unwrap()));
            }
        }
        Event::Encoding { route, .. } => {
            job.phase = Phase::Encoding;
            job.encoder = Some(route.encoder(job.settings.expect("started job").codec));
            job.progress_percent = Some(0);
        }
        Event::Validating => {
            job.phase = Phase::Validating;
            job.progress_percent = None;
        }
        Event::RouteFailed { route, detail } => {
            job.encoder_error = Some(detail.to_owned());
            note(job, format!("{} failed.", route.encoder(job.settings.expect("started job").codec)));
        }
        Event::Oversize {
            measured,
            next_video_bps,
        } => {
            job.phase = Phase::Retrying;
            job.progress_percent = None;
            if next_video_bps.is_some() {
                job.retries += 1;
            }
            note(
                job,
                format!(
                    "Output exceeded limit ({measured} bytes). Retry bitrate: {next_video_bps:?}"
                ),
            );
        }
    }
}

fn media_summary(info: &crate::media::MediaInfo) -> String {
    let mut lines = Vec::new();
    lines.push(format!("Container: {} | Duration: {}",
        info.format.as_deref().unwrap_or("unknown"),
        info.duration_us.map_or_else(|| "unknown".into(), |n| format!("{:.2} s", n as f64 / 1_000_000.0))));
    for s in &info.streams {
        let codec = s.codec.as_deref().unwrap_or("unknown");
        match s.kind.as_deref() {
            Some("video") if !s.attached_picture => {
                let fps = s.avg_rate.map_or_else(|| "unknown fps".into(), |r| format!("{:.3} fps (average)", r.numerator as f64 / r.denominator as f64));
                lines.push(format!("Video: {codec} | {} × {} | {} | {fps} | {:?} | {}",
                    s.width.map_or_else(|| "?".into(), |n| n.to_string()),
                    s.height.map_or_else(|| "?".into(), |n| n.to_string()),
                    s.bit_depth.map_or_else(|| "unknown depth".into(), |n| format!("{n}-bit")),
                    s.hdr, s.pixel_format.as_deref().unwrap_or("unknown pixel format")));
                lines.push(format!("Color: {} / {} / {} / {} | Field order: {}",
                    s.color_primaries.as_deref().unwrap_or("unknown"), s.color_transfer.as_deref().unwrap_or("unknown"),
                    s.color_space.as_deref().unwrap_or("unknown"), s.color_range.as_deref().unwrap_or("unknown"),
                    s.raw["field_order"].as_str().unwrap_or("unknown")));
            }
            Some("audio") => lines.push(format!("Audio: {codec} | {} channels | {} Hz",
                s.channels.map_or_else(|| "?".into(), |n| n.to_string()),
                s.sample_rate.map_or_else(|| "?".into(), |n| n.to_string()))),
            Some("subtitle") => lines.push(format!("Subtitle: {codec}")),
            _ => {}
        }
    }
    lines.join("\n")
}

// Naming uses the settings frozen at Start, not the actual encoded size.
fn output_path(dir: &Path, source: &Path, settings: Settings) -> PathBuf {
    let mut name = source.file_stem().expect("checked on admission").to_os_string();
    let target = match settings.target() {
        Target::Bytes(bytes) => format!("{}M", bytes / 1_048_576),
        Target::Percent(percent) => format!("{percent}%"),
    };
    let codec = match settings.codec() {
        Codec::H264 => "H264",
        Codec::Hevc => "HEVC",
        Codec::Av1 => "AV1",
    };
    name.push(format!("_{target}_{codec}_{}.MP4", settings.max_long_edge()));
    dir.join(name)
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
        // Re-read only filesystem metadata, not media contents. A queued file
        // may have changed since its background size was displayed.
        let meta = fs::metadata(&job.source).map_err(pipeline::Error::Io)?;
        if !meta.is_file() || meta.len() == 0 {
            return Err(pipeline::Error::UnsupportedMedia("Source must be a nonempty file."));
        }
        if cancel.load(Ordering::Acquire) {
            return Err(pipeline::Error::Cancelled);
        }
        let source_bytes = meta.len();
        let settings = job.settings.expect("started job");
        let limit = settings.limit_bytes(source_bytes);
        update.modify(|job| {
            job.source_bytes = Some(source_bytes);
            job.source_size_pending = false;
            job.target_bytes = Some(limit);
        });
        if source_bytes <= limit {
            // Size is the primary goal, regardless of requested codec/edge.
            // Do not create a duplicate, invoke FFmpeg, or write learning data.
            return Ok(None);
        }
        if limit == 0 {
            return Err(pipeline::Error::UnsupportedMedia("Target rounds down to zero bytes."));
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
        let destination = output_path(&dir, &job.source, settings);
        update.modify(|job| job.destination = Some(destination.clone()));
        // Skip existing targets before launching any media or encoder process.
        // The pipeline also protects against a destination created concurrently.
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
        let request = EncodeRequest {
                source: &job.source,
                destination: &destination,
                ffmpeg: &ffmpeg,
                ffprobe: &ffprobe,
                limit_bytes: limit,
                codec: job.settings.expect("started job").codec,
                max_long_edge: job.settings.expect("started job").max_long_edge,
                timeout: Duration::from_secs(86_400),
        };
        let observer = |event: Event<'_>| { update.modify(|job| observe(job, event)); };
        let report = |percent| { update.modify(|job| {
            if job.phase == Phase::Encoding { job.progress_percent = Some(percent); }
        }); };
        let parallel = update.shared.state.lock().unwrap_or_else(|e| e.into_inner()).parallelism > 1;
        let publication = if parallel {
            let output = pipeline::encode_with_progress(&request, detector, cancel, observer, Some(&report))?;
            pipeline::TrackedPublication { output, learning: Ok(()) }
        } else {
            let store = Store::portable(root).map_err(pipeline::Error::Io)?;
            pipeline::encode_and_learn_with_progress(&request, detector, cancel, observer, &store, Some(&report))?
        };
        // One metadata-only read of the saved container, never a frame scan.
        // This is presentation only and cannot turn a saved output into failure.
        match crate::media::probe(&ffprobe, &publication.output.path, cancel) {
            Ok(info) => { update.modify(|job| job.output_info = Some(media_summary(&info))); }
            Err(error) => { update.modify(|job| note(job, format!("Output metadata unavailable: {error}"))); }
        }
        Ok(Some(publication))
    })();
    if let Some(mut completed) = update.modify(|current| current.clone()) {
        let job = &mut completed;
        job.progress_percent = None;
        match result {
            Ok(None) => {
                if cancel.load(Ordering::Acquire) {
                    job.phase = Phase::Cancelled;
                    note(job, "Cancelled during processing.".into());
                } else {
                    job.phase = Phase::Skipped;
                    job.skip_reason = Some("Source already within target capacity");
                    note(job, "Source already within target capacity. Original retained; no output created.".into());
                }
            }
            Ok(Some(publication)) => {
                let out = publication.output;
                job.phase = Phase::Published;
                job.output = Some(out.path);
                job.bytes = Some(out.bytes);
                job.elapsed = Some(out.elapsed);
                job.eta = out.production_eta;
                job.selection = Some(out.selection);
                job.encoder = Some(out.route.encoder(job.settings.expect("started job").codec));
                if let Err(error) = publication.learning {
                    note(
                        job,
                        format!("Output saved. Local learning write failed: {error}"),
                    );
                }
            }
            Err(pipeline::Error::DestinationExists) => {
                if cancel.load(Ordering::Acquire) {
                    job.phase = Phase::Cancelled;
                    note(job, "Cancelled during processing.".into());
                } else {
                    job.phase = Phase::Skipped;
                    job.skip_reason = Some("Target file already exists");
                    note(job, "Target file already exists. Existing output retained; no overwrite.".into());
                }
            }
            Err(pipeline::Error::Cancelled) => {
                job.phase = Phase::Cancelled;
                note(job, "Cancelled during processing.".into());
            }
            Err(error) => {
                job.phase = Phase::Failed;
                let detail = match error {
                    pipeline::Error::Process(detail) => {
                        job.encoder_error = Some(detail);
                        "FFmpeg/process failed".into()
                    }
                    pipeline::Error::HardwareRouteUnavailable => "No compatible hardware route. See the preflight or encoding failure details.".into(),
                    pipeline::Error::Io(error)
                        if error.kind() == io::ErrorKind::PermissionDenied =>
                    {
                        format!("Access denied. Check portable output write permission and input access: {error}")
                    }
                    pipeline::Error::Io(error) => error.to_string(),
                    other => format!("{other:?}"),
                };
                job.failure = Some(detail.clone());
                note(job, detail);
            }
        }
        if let Err(error) = write_log(root, job) {
            note(job, format!("Portable log write failed: {error}"));
        }
        update.modify(|job| {
            // The size reader may have completed while the terminal log was
            // being written; never replace its newer metadata with a stale copy.
            completed.source_bytes = job.source_bytes;
            completed.source_size_pending = job.source_size_pending;
            *job = completed;
        });
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
    fn two_workers_cancel_and_clear_only_after_both_return() {
        let (tx, rx) = mpsc::channel();
        let (release_a, wait_a) = mpsc::channel();
        let (release_b, wait_b) = mpsc::channel();
        let waits = [Mutex::new(wait_a), Mutex::new(wait_b)];
        let queue = Queue::spawn(move |shared| {
            thread::scope(|scope| {
                for (i, wait) in waits.iter().enumerate() {
                    let tx = tx.clone();
                    scope.spawn(move || work(shared, |job, cancel, update| {
                        tx.send((i, job.id)).unwrap();
                        wait.lock().unwrap().recv_timeout(Duration::from_secs(10)).unwrap();
                        assert!(cancel.load(Ordering::Acquire));
                        update.modify(|job| job.phase = Phase::Cancelled);
                    }));
                }
            });
        }).unwrap();
        let root = std::env::temp_dir();
        let a = queue.add(&root.join("a.mp4")).unwrap();
        let b = queue.add(&root.join("b.mp4")).unwrap();
        let duplicate = queue.add(&root.join("a.mkv")).unwrap();
        queue.start(settings()).unwrap();
        let mut ids = [rx.recv_timeout(Duration::from_secs(5)).unwrap().1,
                       rx.recv_timeout(Duration::from_secs(5)).unwrap().1];
        ids.sort();
        assert_eq!(ids, [a, b]);
        assert_eq!(queue.snapshot().iter().find(|j| j.id == duplicate).unwrap().phase, Phase::Ready);
        queue.clear_all();
        release_a.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !queue.snapshot().iter().any(|j| j.id != duplicate && j.phase == Phase::Cancelled) {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(10));
        }
        assert!(queue.is_clearing());
        assert_eq!(queue.snapshot().len(), 3);
        release_b.send(()).unwrap();
        while queue.is_clearing() {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(10));
        }
        assert!(queue.snapshot().is_empty());
        queue.shutdown();
    }

    #[test]
    fn reset_requeues_terminal_jobs_with_fresh_identity_and_settings() {
        let queue = Queue::spawn(|shared| work(shared, |_, _, _| unreachable!())).unwrap();
        let source = std::env::temp_dir().join("小K 的鹅腿坟场 & (黑洞) ^ % !.avi");
        for phase in [Phase::Published, Phase::Failed, Phase::Cancelled, Phase::Skipped] {
            let id = queue.add(&source).unwrap();
            assert!(queue.reset(id).is_err());
            let update = Updates { shared: &queue.shared, id };
            update.modify(|job| {
                job.phase = phase;
                job.settings = Some(settings());
                job.target_bytes = Some(100);
                job.progress_percent = Some(100);
                job.output = Some(source.clone());
                job.bytes = Some(50);
                job.retries = 2;
                job.failure = Some("old failure".into());
                job.skip_reason = Some("old skip");
                job.encoder_error = Some("old stderr".into());
                job.detail.push("old detail".into());
            });
            queue.reset(id).unwrap();
            let jobs = queue.snapshot();
            let job = &jobs[0];
            assert_ne!(job.id,id);
            assert_eq!(job.source,source);
            assert_eq!(job.phase,Phase::Waiting);
            assert!(job.skip_reason.is_none() && job.settings.is_none() && job.target_bytes.is_none());
            assert!(job.progress_percent.is_none() && job.output.is_none() && job.bytes.is_none());
            assert!(job.failure.is_none() && job.encoder_error.is_none() && job.detail.is_empty());
            assert_eq!(job.retries,0);
            assert!(update.modify(|_| panic!("stale attempt updated a new row")).is_none());
            queue.cancel(job.id);
            queue.clear_finished();
        }
        queue.shutdown();
    }

    #[test]
    fn parameter_output_names_and_duplicate_skip_without_tools() {
        let root = std::env::temp_dir().join(format!("dotorca-names-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let source = root.join("小K 的鹅腿坟场 & (黑洞) ^ % !.avi");
        fs::File::create(&source).unwrap().set_len(2_000_000).unwrap();
        let pct = Settings::percentage(50, Codec::Hevc, 1920).unwrap();
        let fixed = Settings::new(100, Codec::Av1, 3840).unwrap();
        let dir = root.join("output");
        fs::create_dir(&dir).unwrap();
        let target = output_path(&dir,&source,pct);
        assert_eq!(target.file_name().unwrap().to_str().unwrap(),"小K 的鹅腿坟场 & (黑洞) ^ % !_50%_HEVC_1920.MP4");
        assert!(output_path(&dir,&source,fixed).ends_with("小K 的鹅腿坟场 & (黑洞) ^ % !_100M_AV1_3840.MP4"));
        fs::write(&target,b"previous output").unwrap();
        let queue = Queue::portable(root.clone()).unwrap();
        queue.add(&source).unwrap();
        queue.start(pct).unwrap();
        let jobs = await_finished(&queue);
        assert_eq!(jobs[0].phase,Phase::Skipped);
        assert_eq!(jobs[0].skip_reason,Some("Target file already exists"));
        assert_eq!(jobs[0].destination.as_ref(),Some(&target));
        assert!(jobs[0].encoder.is_none() && jobs[0].output.is_none());
        queue.reset(jobs[0].id).unwrap();
        queue.start(pct).unwrap();
        assert_eq!(await_finished(&queue)[0].skip_reason,Some("Target file already exists"));
        assert_eq!(fs::read(&target).unwrap(),b"previous output");
        assert_eq!(fs::read_dir(&dir).unwrap().count(),1);
        queue.shutdown();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn percentage_targets_are_exact_and_bounded() {
        let settings = Settings::percentage(60, Codec::Hevc, 1920).unwrap();
        assert_eq!(settings.limit_bytes(101), 60);
        assert_eq!(settings.limit_bytes(u64::MAX), (u128::from(u64::MAX) * 60 / 100) as u64);
        assert!(Settings::percentage(0, Codec::Hevc, 1920).is_err());
        assert!(Settings::percentage(101, Codec::Hevc, 1920).is_err());
        assert_eq!(Settings::percentage(100, Codec::Hevc, 1920).unwrap().limit_bytes(u64::MAX), u64::MAX);
    }

    #[test]
    fn already_small_files_skip_without_tools_or_outputs() {
        let root = std::env::temp_dir().join(format!("dotorca-skip-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let path = root.join("小K 的鹅腿坟场 & (黑洞) ^ % !.avi");
        let equal = root.join("equal.mp4");
        let big = root.join("big.mp4");
        fs::write(&path, [0u8; 100]).unwrap();
        fs::File::create(&equal).unwrap().set_len(1_048_576).unwrap();
        fs::File::create(&big).unwrap().set_len(1_048_577).unwrap();
        let queue = Queue::portable(root.clone()).unwrap();
        for p in [&path, &equal, &big] { queue.add(p).unwrap(); }
        queue.start(settings()).unwrap();
        let jobs = await_finished(&queue);
        assert_eq!(jobs[0].phase, Phase::Skipped);
        assert_eq!(jobs[1].phase, Phase::Skipped);
        assert_eq!(jobs[2].phase, Phase::Failed); // no encoding tools
        assert!(jobs[0].output.is_none() && jobs[0].encoder.is_none());
        assert_eq!(jobs[1].target_bytes, Some(1_048_576));
        queue.clear_finished();
        queue.add(&path).unwrap();
        queue.start(Settings::percentage(100, Codec::Hevc, 1920).unwrap()).unwrap();
        assert_eq!(await_finished(&queue)[0].phase, Phase::Skipped);
        queue.clear_finished();
        queue.add(&path).unwrap();
        queue.start(Settings::percentage(50, Codec::Hevc, 1920).unwrap()).unwrap();
        let jobs = await_finished(&queue);
        assert_eq!(jobs[0].target_bytes, Some(50));
        assert_eq!(jobs[0].phase, Phase::Failed);
        assert!(fs::read_dir(root.join("output")).unwrap().next().is_none());
        assert!(!root.join("data/local/production-v1.json").exists());
        queue.shutdown();
        assert_eq!(fs::metadata(&path).unwrap().len(), 100);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn source_sizes_arrive_without_start_and_handle_missing_paths() {
        let root = std::env::temp_dir().join(format!("dotorca-size-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("小K 的鹅腿坟场 & (黑洞) ^ % !.avi");
        fs::write(&path, [0u8; 1234]).unwrap();
        let queue = Queue::spawn(|shared| work(shared, |_, _, _| unreachable!())).unwrap();
        queue.add(&path).unwrap();
        queue.add(&root.join("-missing 🎬.mp4")).unwrap();
        queue.add(&root).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let jobs = queue.snapshot();
            assert!(jobs.iter().all(|j| j.phase == Phase::Waiting));
            if jobs.iter().all(|j| !j.source_size_pending) {
                assert_eq!(jobs[0].source_bytes, Some(1234));
                assert_eq!(jobs[1].source_bytes, None);
                assert_eq!(jobs[2].source_bytes, None);
                break;
            }
            assert!(Instant::now() < deadline, "{jobs:?}");
            thread::sleep(Duration::from_millis(10));
        }
        queue.shutdown();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn retry_and_encoder_error_keep_diagnostic() {
        let queue = Queue::spawn(|shared| work(shared, |_, _, _| unreachable!())).unwrap();
        let id = queue.add(&std::env::temp_dir().join("test.mp4")).unwrap();
        let update = Updates { shared: &queue.shared, id };
        let error = format!("{}\nactual encoder error", "x".repeat(500));
        update.modify(|job| {
            job.settings = Some(settings());
            job.progress_percent = Some(100);
            observe(job, Event::Oversize { measured: 2000000, next_video_bps: Some(100000) });
            assert_eq!(job.phase, Phase::Retrying);
            assert_eq!(job.progress_percent, None);
            assert_eq!(job.retries, 1);
            observe(job, Event::RouteFailed { route: crate::decision::Route::Software, detail: &error });
            assert_eq!(job.encoder_error.as_deref(), Some(error.as_str()));
        });
        queue.shutdown();
    }

    #[test]
    fn start_snapshots_settings_and_clear_all_waits_for_worker_cleanup() {
        let (started, running) = mpsc::sync_channel(1);
        let (release, resume) = mpsc::sync_channel(1);
        let resume = Mutex::new(resume);
        let queue = Queue::spawn(move |shared| {
            work(shared, |job, cancel, update| {
                started.send(job.settings.unwrap()).unwrap();
                resume.lock().unwrap().recv_timeout(Duration::from_secs(10)).unwrap();
                assert!(cancel.load(Ordering::Acquire));
                update.modify(|job| job.phase = Phase::Cancelled);
            });
        }).unwrap();
        let path = std::env::temp_dir().join("小K 的鹅腿坟场 & (黑洞) ^ % !.avi");
        let first = queue.add(&path).unwrap();
        queue.add(&path).unwrap();
        assert!(queue.snapshot().iter().all(|j| j.settings.is_none()));
        let chosen = Settings::new(30, Codec::Hevc, 1920).unwrap();
        queue.start(chosen).unwrap();
        let actual = running.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(actual.limit_bytes(0) / 1_048_576, 30);
        assert_eq!(actual.codec(), Codec::Hevc);
        assert_eq!(actual.max_long_edge(), 1920);
        let later = queue.add(&path).unwrap();
        assert!(queue.snapshot().iter().find(|j| j.id == later).unwrap().settings.is_none());
        queue.start(settings()).unwrap();
        let jobs = queue.snapshot();
        assert_eq!(jobs[0].settings.unwrap().limit_bytes(0) / 1_048_576, 30);
        assert_eq!(jobs[1].settings.unwrap().limit_bytes(0) / 1_048_576, 30);
        assert_eq!(jobs[2].settings.unwrap().limit_bytes(0) / 1_048_576, 1);
        queue.clear_all();
        assert!(queue.is_clearing());
        let jobs = queue.snapshot();
        assert_eq!(jobs[0].id, first);
        assert_eq!(jobs[0].phase, Phase::Cancelling);
        assert!(jobs[1..].iter().all(|j| j.phase == Phase::Cancelled));
        assert!(queue.add(&path).is_err());
        assert!(queue.start(settings()).is_err());
        release.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while queue.is_clearing() {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(10));
        }
        assert!(queue.snapshot().is_empty());
        assert!(queue.add(&path).unwrap() > later);
        queue.clear_all();
        assert!(queue.snapshot().is_empty());
        queue.shutdown();
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
        let active = queue.add(&source).unwrap();
        queue.start(settings()).unwrap();
        running.recv_timeout(Duration::from_secs(5)).unwrap();
        let snapshot = queue.snapshot();
        let waiting = queue.add(&source).unwrap();
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
            let original = fs::canonicalize(std::env::var_os(key).unwrap()).unwrap();
            let local = tools.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
            // Linux can return ETXTBSY when a freshly copied executable is
            // still held open by a transient reader. Link the CI fixture
            // instead; Windows still tests the actual portable copied tools.
            #[cfg(unix)]
            std::os::unix::fs::symlink(original, local).unwrap();
            #[cfg(not(unix))]
            fs::copy(original, local).unwrap();
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
                    "rawvideo",
                    "-pix_fmt",
                    "yuv420p",
                    "-f",
                    "avi",
                ])
                .args([source.as_os_str()]),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(output.status.success(), "{:?}", output.stderr);
        let queue_root = root.clone();
        let queue = Queue::spawn(move |shared| {
            crate::decision::allow_fixture_fallback();
            let mut detector = Detector::default();
            work(shared, |job, cancel, update| {
                encode(&queue_root, &mut detector, job, cancel, update)
            });
        })
        .unwrap();
        assert!(fs::metadata(&source).unwrap().len() > 1_048_576);
        queue.add(&source).unwrap();
        queue.add(&source).unwrap();
        queue
            .add(&root.join("-missing 🎬.mp4"))
            .unwrap();
        let cancelled = queue.add(&root.join("cancelled.mp4")).unwrap();
        queue.cancel(cancelled);
        queue.start(settings()).unwrap();

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
        assert_eq!(jobs[1].phase, Phase::Skipped);
        assert_eq!(jobs[1].skip_reason, Some("Target file already exists"));
        assert!(jobs[1].destination.is_some());
        assert!(jobs[0].source_info.as_deref().unwrap().contains("320 × 180"));
        assert!(jobs[0].target_info.as_deref().unwrap().contains("256 × 144"));
        assert!(jobs[0].output_info.as_deref().unwrap().contains("256 × 144"));
        assert_eq!(jobs[2].phase, Phase::Failed);
        assert_eq!(jobs[3].phase, Phase::Cancelled);
        assert!(source.exists());
        assert!(
            root.join("data/local/production-v1.json").is_file(),
            "{:?}",
            jobs[0]
        );
        queue.clear_all();
        assert!(queue.snapshot().is_empty());
        assert!(source.exists());
        assert!(jobs[0].output.as_ref().unwrap().is_file());
        // Same settings skip; reset and change settings to encode separately.
        let repeat_id = queue.add(&source).unwrap();
        queue.start(settings()).unwrap();
        assert_eq!(await_finished(&queue)[0].phase, Phase::Skipped);
        queue.reset(repeat_id).unwrap();
        let changed = Settings::new(1, Codec::H264, 192).unwrap();
        queue.start(changed).unwrap();
        let repeat = await_finished(&queue);
        assert_eq!(repeat[0].phase, Phase::Published, "{:?}",repeat[0]);
        assert_ne!(repeat[0].output,jobs[0].output);
        assert!(repeat[0].output.as_ref().unwrap().ends_with("小K 的鹅腿坟场 & (黑洞) ^ % !_1M_H264_192.MP4"));
        assert!(repeat[0].target_info.as_deref().unwrap().contains("192 × 108"));
        assert!(jobs[0].output.as_ref().unwrap().is_file());
        queue.clear_finished();
        let percent_source = root.join("percent 🎬.avi");
        fs::copy(&source, &percent_source).unwrap();
        let source_bytes = fs::metadata(&percent_source).unwrap().len();
        queue.add(&percent_source).unwrap();
        queue.start(Settings::percentage(25, Codec::H264, 256).unwrap()).unwrap();
        let percent = await_finished(&queue);
        assert_eq!(percent[0].phase, Phase::Published, "{:?}", percent[0]);
        assert_eq!(percent[0].target_bytes, Some(source_bytes / 4));
        assert!(percent[0].bytes.unwrap() <= source_bytes / 4);
        queue.shutdown();
    }
}
