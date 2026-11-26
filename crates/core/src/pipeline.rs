//! S06 encoding, validation and no-clobber publication.
use crate::{
    budget::{self, Codec, Plan},
    dataset::{Fingerprint, Observation, Outcome, Profile, Store},
    decision::{self, CheckResult, Decision, Route, Selection},
    hardware::Detector,
    media::{self, Hdr, MediaInfo},
    process::{self, Request, Stop},
};
use std::{
    fs::{self, File, Metadata},
    io::{self, Read},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::{Duration, Instant},
};

pub mod eta;
mod timeline;
#[cfg(windows)]
#[allow(unsafe_code)]
mod windows;

const MAX_REENCODES: usize = 2;

#[derive(Debug)]
pub struct EncodeRequest<'a> {
    pub source: &'a Path,
    pub destination: &'a Path,
    pub ffmpeg: &'a Path,
    pub ffprobe: &'a Path,
    pub limit_bytes: u64,
    pub codec: Codec,
    pub max_long_edge: u32,
    /// Per subprocess deadline. Preflight additionally caps its own deadline.
    pub timeout: Duration,
}

#[derive(Debug)]
pub enum Error {
    Budget(budget::Error),
    Cancelled,
    DiskFull,
    DestinationExists,
    InvalidDestination,
    SourceChanged,
    UnsupportedDynamicHdr,
    UnsupportedMedia(&'static str),
    Io(io::Error),
    Process(String),
    NoRoute,
    HardwareRouteUnavailable,
    CapacityExceeded { limit: u64, measured: u64 },
}

#[derive(Debug)]
pub enum Event<'a> {
    Decision(&'a Decision<'a>),
    Preflight {
        route: Route,
        video_bps: u64,
        result: CheckResult,
        detail: &'a str,
    },
    Encoding {
        route: Route,
        video_bps: u64,
    },
    Validating,
    RouteFailed {
        route: Route,
        detail: &'a str,
    },
    Oversize {
        measured: u64,
        next_video_bps: Option<u64>,
    },
}

#[derive(Debug)]
pub struct Published {
    pub path: PathBuf,
    pub bytes: u64,
    pub route: Route,
    pub selection: Selection,
    pub video_bps: u64,
    pub elapsed: Duration,
    pub production_eta: Option<eta::Estimate>,
}

fn check_cancel(cancel: &AtomicBool) -> Result<(), Error> {
    if cancel.load(Ordering::Acquire) {
        return Err(Error::Cancelled);
    }
    Ok(())
}

fn io_error(error: io::Error, cancel: &AtomicBool) -> Error {
    if cancel.load(Ordering::Acquire) {
        Error::Cancelled
    } else if matches!(
        error.kind(),
        io::ErrorKind::StorageFull | io::ErrorKind::QuotaExceeded
    ) {
        Error::DiskFull
    } else {
        Error::Io(error)
    }
}

fn check_destination(path: &Path) -> Result<(), Error> {
    match fs::symlink_metadata(path) {
        Ok(_) => Err(Error::DestinationExists),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(Error::Io(e)),
    }
}

/// A private staging directory beside the destination keeps FFmpeg's -n and
/// the final publication on the same filesystem, including with competing jobs.
struct Stage(PathBuf);

impl Stage {
    fn create(parent: &Path) -> io::Result<Self> {
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        for _ in 0..256 {
            let id = SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = parent.join(format!(".dotorca-{}-{id}", std::process::id()));
            let mut builder = fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            match builder.create(&path) {
                Ok(()) => return Ok(Self(path)),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "staging names exhausted",
        ))
    }
}

impl Drop for Stage {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn publish(staged: &Path, destination: &Path, cancel: &AtomicBool) -> Result<(), Error> {
    // The muxer and probe are closed. Flush the validated file before making
    // its name visible. The platform operation publishes without replacing.
    File::options()
        .write(true)
        .open(staged)
        .and_then(|file| file.sync_all())
        .map_err(|e| io_error(e, cancel))?;
    check_cancel(cancel)?;
    #[cfg(windows)]
    let result = windows::publish(staged, destination);
    #[cfg(not(windows))]
    let result = fs::hard_link(staged, destination);
    result.map_err(|e| {
        if e.kind() == io::ErrorKind::AlreadyExists {
            Error::DestinationExists
        } else {
            io_error(e, cancel)
        }
    })
}

fn unchanged(path: &Path, original: &Metadata, cancel: &AtomicBool) -> Result<(), Error> {
    let now = fs::metadata(path).map_err(|e| io_error(e, cancel))?;
    if !now.is_file()
        || now.len() != original.len()
        || now.modified().ok() != original.modified().ok()
    {
        return Err(Error::SourceChanged);
    }
    Ok(())
}

fn retry_bitrate(plan: &Plan<'_>, current: u64, measured: u64) -> Option<u64> {
    let nonvideo = plan.audio_bytes.checked_add(plan.mux_reserve_bytes)?;
    let available = plan.limit_bytes.checked_sub(nonvideo)?;
    let observed_video = measured.checked_sub(nonvideo)?;
    // Keep all stream constraints. Reserve another 5% for rate-control error.
    let reduced =
        u128::from(current) * (u128::from(available) * 95 / 100) / u128::from(observed_video);
    u64::try_from(reduced)
        .ok()
        .filter(|&r| r >= 1_000 && r < current)
}

fn encode_command(
    request: &EncodeRequest<'_>,
    source: &MediaInfo,
    plan: &Plan<'_>,
    clock: timeline::Clock,
    interlaced: bool,
    route: Route,
    rate: u64,
    path: &Path,
) -> Request {
    let planar = route == Route::Software || route == Route::Amf;
    let pix = match (plan.source.bit_depth, planar) {
        (Some(10), true) if route == Route::Software => "yuv420p10le",
        (Some(10), _) => "p010le",
        (_, true) => "yuv420p",
        _ => "nv12",
    };
    let range = match plan.source.color_range.as_deref() {
        Some("pc") => ":in_range=pc:out_range=pc",
        Some("tv") => ":in_range=tv:out_range=tv",
        _ => "",
    };
    // scale adjusts SAR to keep the original DAR. Explicit format conversion
    // is required because strict +pix_fmt disables auto-inserted converters.
    let deinterlace = if interlaced {
        "bwdif=mode=send_field:parity=auto:deint=all,"
    } else {
        ""
    };
    let filter = format!(
        "{deinterlace}scale={}:{}:flags=lanczos{range},format={pix}",
        plan.width, plan.height
    );
    let timescale = clock.den.to_string();
    let enc_time_base = format!("1:{timescale}");
    let video_rate = rate.to_string();
    let movie_timescale = clock.movie_timescale().to_string();
    let mut cmd = Request::new(request.ffmpeg)
        .args([
            "-hide_banner",
            "-nostdin",
            "-n",
            "-loglevel",
            "error",
            "-xerror",
            "-abort_on",
            "empty_output",
            "-copyts",
            "-noautorotate",
            "-i",
        ])
        .args([source.path.as_os_str()])
        .args([
            "-map",
            "0:V:0",
            "-c:v",
            route.encoder(plan.codec),
            "-vf",
            &filter,
            "-pix_fmt",
            &format!("+{pix}"),
            "-fps_mode:v",
            "passthrough",
            "-enc_time_base:v",
            &enc_time_base,
            "-b:v",
            &video_rate,
        ]);
    // SVT selects random-access VBR from the target bitrate alone. A
    // maxrate equal to the target selects unsupported CBR on older wrappers.
    if !(route == Route::Software && plan.codec == Codec::Av1) {
        let ceiling = if route == Route::Qsv {
            rate.saturating_add(rate.div_ceil(5))
        } else {
            rate
        };
        cmd = cmd.args([
            "-maxrate:v",
            &ceiling.to_string(),
            "-bufsize:v",
            &rate.saturating_mul(2).to_string(),
        ]);
    }
    match route {
        Route::Nvenc => cmd = cmd.args(["-preset", "p4", "-rc", "vbr"]),
        Route::Amf => cmd = cmd.args(["-rc", "vbr_peak"]),
        Route::Software => {
            cmd = match plan.codec {
                Codec::H264 | Codec::Hevc => cmd.args(["-preset", "fast"]),
                Codec::Av1 => cmd.args(["-preset", "8"]),
            };
        }
        Route::Qsv => {}
    }
    for (option, value) in [
        ("-color_range", plan.source.color_range.as_deref()),
        ("-colorspace", plan.source.color_space.as_deref()),
        ("-color_trc", plan.source.color_transfer.as_deref()),
        ("-color_primaries", plan.source.color_primaries.as_deref()),
        (
            "-chroma_sample_location",
            plan.source.raw["chroma_location"]
                .as_str()
                .filter(|v| !matches!(*v, "unspecified" | "unknown")),
        ),
    ] {
        if let Some(value) = value {
            cmd = cmd.args([option, value]);
        }
    }
    if plan.codec == Codec::Av1 {
        // Older encoder wrappers omit this metadata even with the codec
        // option set. AV1 has exact equivalents for left and top-left.
        let chroma_position = match plan.source.raw["chroma_location"].as_str() {
            Some("left") => Some("vertical"),
            Some("topleft") => Some("colocated"),
            _ => None,
        };
        if let Some(position) = chroma_position {
            cmd = cmd.args([
                "-bsf:v",
                &format!("av1_metadata=chroma_sample_position={position}"),
            ]);
        }
    }
    if !plan.audio_bps.is_empty() {
        cmd = cmd.args(["-map", "0:a", "-c:a", "aac"]);
        for (index, rate) in plan.audio_bps.iter().enumerate() {
            cmd = cmd.args([format!("-b:a:{index}"), rate.to_string()]);
        }
    }
    for stream in &source.streams {
        if stream.kind.as_deref() == Some("subtitle")
            && matches!(
                stream.codec.as_deref(),
                Some("subrip" | "ass" | "ssa" | "mov_text" | "webvtt" | "text")
            )
        {
            if let Some(index) = stream.index {
                cmd = cmd.args(["-map", &format!("0:{index}")]);
            }
        }
    }
    cmd = cmd.args(["-c:s", "mov_text"]);
    cmd = cmd
        .args([
            "-map_metadata",
            "-1",
            "-map_chapters",
            "-1",
            "-avoid_negative_ts",
            "disabled",
            "-video_track_timescale",
            &timescale,
            "-movie_timescale",
            &movie_timescale,
            "-use_editlist",
            "1",
            "-f",
            "mp4",
        ])
        .args([path.as_os_str()]);
    cmd.timeout = request.timeout;
    cmd
}

fn run_encoder(cmd: &Request, encoder: &str, cancel: &AtomicBool) -> Result<(), Error> {
    finish_encoder(process::run(cmd, cancel), encoder, cancel)
}

fn finish_encoder(
    result: io::Result<process::Outcome>,
    encoder: &str,
    cancel: &AtomicBool,
) -> Result<(), Error> {
    let out = result.map_err(|e| io_error(e, cancel))?;
    check_cancel(cancel)?;
    if out.stop == Stop::TimedOut {
        return Err(Error::Process(format!(
            "{encoder}: encoding deadline exceeded"
        )));
    }
    let stderr = String::from_utf8_lossy(&out.stderr.bytes);
    if stderr
        .to_ascii_lowercase()
        .contains("no space left on device")
        || stderr.to_ascii_lowercase().contains("disk quota exceeded")
        || stderr
            .to_ascii_lowercase()
            .contains("not enough space on the disk")
    {
        return Err(Error::DiskFull);
    }
    if out.status.success() {
        return Ok(());
    }
    let detail = format!("{encoder}: {}: {}", out.status, stderr);
    Err(Error::Process(detail))
}

/// FFmpeg progress records are line oriented. Keep a fixed-size line buffer
/// and discard long fields while continuing to drain the pipe.
fn read_progress(mut input: impl Read, total: u64, report: &dyn Fn(u8)) {
    let mut chunk = [0; 4096];
    let mut line = [0; 64];
    let mut len = 0;
    let mut last = 0;
    while let Ok(n @ 1..) = input.read(&mut chunk) {
        for &byte in &chunk[..n] {
            if byte == b'\n' {
                if total != 0 && len <= line.len() {
                    let timestamp = std::str::from_utf8(&line[..len])
                        .ok()
                        .and_then(|s| s.strip_prefix("out_time_us="))
                        .and_then(|s| s.trim().parse::<u64>().ok());
                    if let Some(timestamp) = timestamp {
                        let percent =
                            (100 * u128::from(timestamp) / u128::from(total)).min(99) as u8;
                        if percent > last {
                            last = percent;
                            report(percent);
                        }
                    }
                }
                len = 0;
            } else if len < line.len() {
                line[len] = byte;
                len += 1;
            } else {
                len = line.len() + 1;
            }
        }
    }
}

fn remove(path: &Path, cancel: &AtomicBool) -> Result<(), Error> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(io_error(e, cancel)),
    }
}

/// Run a complete single-file job. The observer receives route decisions and
/// attempt evidence on the caller's worker thread.
pub fn encode(
    request: &EncodeRequest<'_>,
    detector: &mut Detector,
    cancel: &AtomicBool,
    observe: impl FnMut(Event<'_>),
) -> Result<Published, Error> {
    encode_inner(request, detector, cancel, observe, None, None)
}

#[derive(Debug)]
pub struct TrackedPublication {
    pub output: Published,
    pub learning: io::Result<()>,
}

/// Runs the S06 pipeline and records only a verified, successfully
/// published result. A storage failure leaves the output successful.
pub fn encode_and_learn(
    request: &EncodeRequest<'_>,
    detector: &mut Detector,
    cancel: &AtomicBool,
    observe: impl FnMut(Event<'_>),
    store: &Store,
) -> Result<TrackedPublication, Error> {
    encode_and_learn_with_progress(request, detector, cancel, observe, store, None)
}

/// Reports measured frame completion within the current encode attempt.
/// The callback runs on the stdout reader and must return promptly.
pub fn encode_and_learn_with_progress(
    request: &EncodeRequest<'_>,
    detector: &mut Detector,
    cancel: &AtomicBool,
    observe: impl FnMut(Event<'_>),
    store: &Store,
    progress: Option<&(dyn Fn(u8) + Sync)>,
) -> Result<TrackedPublication, Error> {
    check_cancel(cancel)?;
    let fingerprint = Fingerprint::read(request.ffmpeg, request.ffprobe);
    let mut observation = None;
    let tracking = fingerprint.as_ref().ok().map(|id| (id, &mut observation));
    let output = encode_inner(request, detector, cancel, observe, tracking, progress)?;
    let learning = fingerprint.and_then(|before| {
        if before != Fingerprint::read(request.ffmpeg, request.ffprobe)? {
            return Err(io::Error::other("encoding tools changed during production"));
        }
        store.record(observation.expect("tracked publication creates a production observation"))
    });
    Ok(TrackedPublication { output, learning })
}

fn encode_inner(
    request: &EncodeRequest<'_>,
    detector: &mut Detector,
    cancel: &AtomicBool,
    mut observe: impl FnMut(Event<'_>),
    tracking: Option<(&Fingerprint, &mut Option<Observation>)>,
    progress: Option<&(dyn Fn(u8) + Sync)>,
) -> Result<Published, Error> {
    let start = Instant::now();
    check_cancel(cancel)?;
    let source_path = fs::canonicalize(request.source).map_err(|e| io_error(e, cancel))?;
    let source_meta = fs::metadata(&source_path).map_err(|e| io_error(e, cancel))?;
    let source =
        media::probe(request.ffprobe, &source_path, cancel).map_err(|e| io_error(e, cancel))?;
    let plan = budget::plan(
        &source,
        request.limit_bytes,
        request.codec,
        request.max_long_edge,
    )
    .map_err(Error::Budget)?;
    if plan.source.hdr == Hdr::DolbyVision || timeline::dynamic_hdr(&plan.source.raw.to_string()) {
        return Err(Error::UnsupportedDynamicHdr);
    }
    let name = request
        .destination
        .file_name()
        .filter(|_| {
            request
                .destination
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("mp4"))
        })
        .ok_or(Error::InvalidDestination)?;
    let directory = request
        .destination
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let directory = fs::canonicalize(directory).map_err(|e| io_error(e, cancel))?;
    let destination = directory.join(name);
    check_destination(&destination)?;
    unchanged(&source.path, &source_meta, cancel)?;
    let interlaced = matches!(
        plan.source.raw["field_order"].as_str(),
        Some("tt" | "bb" | "tb" | "bt")
    );
    let mut clock = timeline::Clock::from_stream(plan.source)?;
    if interlaced {
        clock.den = clock.den.saturating_mul(2);
    }
    if clock.den > i32::MAX as u64 {
        return Err(Error::UnsupportedMedia(
            "video time base exceeds MP4 timescale",
        ));
    }
    #[cfg(not(test))]
    let hardware = detector.list_for_transcode(request.ffmpeg, cancel);
    #[cfg(test)]
    let hardware = if decision::fixture_fallback() {
        detector.detect(request.ffmpeg, cancel)
    } else {
        detector.list_for_transcode(request.ffmpeg, cancel)
    };
    let hardware = hardware.map_err(|e| io_error(e, cancel))?;
    let decision = decision::decide(Ok(&plan), &hardware, &[], None).map_err(Error::Budget)?;
    observe(Event::Decision(&decision));
    let route = match decision.selection {
        Selection::DefaultHardware(route) => route,
        #[cfg(test)]
        Selection::SoftwareFallback(_) => Route::Software,
        _ => return Err(Error::HardwareRouteUnavailable),
    };

    let stage = Stage::create(&directory).map_err(|e| io_error(e, cancel))?;
    let output = stage.0.join("encoded.mp4");
    let mut rate = plan.video_bps;
    for reencodes in 0..=MAX_REENCODES {
        check_cancel(cancel)?;
        unchanged(&source.path, &source_meta, cancel)?;
        check_destination(&destination)?;
        observe(Event::Encoding {
            route,
            video_bps: rate,
        });
        let attempt_start = Instant::now();
        let mut command = encode_command(
            request, &source, &plan, clock, interlaced, route, rate, &output,
        );
        let encoder = route.encoder(plan.codec);
        let result = if let Some(report) = progress {
            command
                .args
                .splice(0..0, ["-progress".into(), "pipe:1".into()]);
            let result = process::run_with_stdout(&command, cancel, |reader| {
                read_progress(reader, plan.duration_us, report);
            });
            finish_encoder(result.map(|(outcome, ())| outcome), encoder, cancel)
        } else {
            run_encoder(&command, encoder, cancel)
        };
        if let Err(Error::Process(detail)) = &result {
            observe(Event::RouteFailed { route, detail });
        }
        result?;
        let encode_elapsed = attempt_start.elapsed();
        observe(Event::Validating);
        let bytes = fs::metadata(&output)
            .map_err(|e| io_error(e, cancel))?
            .len();
        if bytes == 0 {
            return Err(Error::Process("FFmpeg produced an empty file".into()));
        }
        if budget::fits(plan.limit_bytes, Some(bytes)) {
            unchanged(&source.path, &source_meta, cancel)?;
            publish(&output, &destination, cancel)?;
            let elapsed = start.elapsed();
            if let Some((tools, slot)) = tracking {
                let profile = Profile::new(
                    &source,
                    &plan,
                    &hardware.inventory,
                    route,
                    rate,
                    tools.clone(),
                );
                *slot = Some(Observation::published(
                    profile,
                    Outcome {
                        total: elapsed,
                        encode: encode_elapsed,
                        bytes,
                        retries: reencodes,
                        failed_preflights: Vec::new(),
                        failed_encodes: Vec::new(),
                        selection: decision.selection,
                    },
                ));
            }
            return Ok(Published {
                path: destination,
                bytes,
                route,
                selection: decision.selection,
                video_bps: rate,
                elapsed,
                production_eta: None,
            });
        }
        let next = (reencodes < MAX_REENCODES)
            .then(|| retry_bitrate(&plan, rate, bytes))
            .flatten();
        observe(Event::Oversize {
            measured: bytes,
            next_video_bps: next,
        });
        rate = next.ok_or(Error::CapacityExceeded {
            limit: plan.limit_bytes,
            measured: bytes,
        })?;
        remove(&output, cancel)?;
    }
    unreachable!("the last capacity retry returns a result")
}

#[cfg(test)]
mod tests;
