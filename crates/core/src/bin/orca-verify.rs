//! S09 developer probe. Uses the production pipeline without writing learning data.
use orca_core::{
    budget::Codec,
    hardware::{self, Detector},
    pipeline::{encode, EncodeRequest, Event},
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    ffi::OsStr,
    fs::File,
    io::{self, BufWriter, Write},
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

const USAGE: &str = "Usage: orca-verify <source> <new-output.mp4> <h264|hevc|av1> <MiB> <max-edge>";

fn hash(path: &Path) -> io::Result<String> {
    let mut sha = Sha256::new();
    io::copy(&mut File::open(path)?, &mut sha)?;
    Ok(format!("{:x}", sha.finalize()))
}

fn positive(value: &OsStr) -> Option<u64> {
    value.to_str()?.parse().ok().filter(|&n| n > 0)
}

fn run() -> Result<bool, Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let [source, destination, codec, mib, edge] = args.as_slice() else {
        return Err(USAGE.into());
    };
    let codec = match codec.to_str() {
        Some("h264") => Codec::H264,
        Some("hevc") => Codec::Hevc,
        Some("av1") => Codec::Av1,
        _ => return Err(USAGE.into()),
    };
    let limit_bytes = positive(mib)
        .and_then(|n| n.checked_mul(1_048_576))
        .ok_or(USAGE)?;
    let max_long_edge = positive(edge)
        .and_then(|n| u32::try_from(n).ok())
        .filter(|&n| n >= 2)
        .ok_or(USAGE)?;
    let exe = std::env::current_exe()?;
    let tools = exe
        .parent()
        .ok_or("Executable directory unavailable")?
        .join("tools");
    let tool = |name| tools.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
    let ffmpeg = tool("ffmpeg");
    let ffprobe = tool("ffprobe");
    let request = EncodeRequest {
        source: Path::new(source),
        destination: Path::new(destination),
        ffmpeg: &ffmpeg,
        ffprobe: &ffprobe,
        limit_bytes,
        codec,
        max_long_edge,
        timeout: Duration::from_secs(3600),
    };
    let input = [PathBuf::from(source), ffmpeg.clone(), ffprobe.clone()];
    let fingerprints = input
        .iter()
        .map(|path| hash(path))
        .collect::<io::Result<Vec<_>>>()?;
    let inventory = hardware::inventory();
    let adapters: Vec<_> = inventory
        .adapters
        .iter()
        .map(|a| json!({"name": a.name, "driver": a.driver_version}))
        .collect();
    let mut input_profile = Value::Null;
    let mut events = Vec::new();
    let start = Instant::now();
    let result = encode(
        &request,
        &mut Detector::default(),
        &AtomicBool::new(false),
        |event| {
            let event = match event {
                Event::Source(_) => json!({"kind": "source_metadata"}),
                Event::Plan(_) => json!({"kind": "capacity_plan"}),
                Event::Decision(decision) => {
                    let plan = decision.job.plan;
                    if input_profile.is_null() {
                        input_profile = json!({
                            "duration_us": plan.duration_us,
                            "source_codec": plan.source.codec,
                            "source_width": plan.source.width,
                            "source_height": plan.source.height,
                            "source_bit_depth": plan.source.bit_depth,
                            "source_hdr": format!("{:?}", plan.source.hdr),
                            "output_width": plan.width,
                            "output_height": plan.height,
                            "audio_bps": plan.audio_bps,
                            "ffmpeg_version": decision.job.hardware.tool_version.lines().next(),
                        });
                    }
                    let candidates: Vec<_> = decision
                        .candidates
                        .iter()
                        .map(|candidate| {
                            json!({
                                "encoder": candidate.encoder,
                                "capability": format!("{:?}", candidate.capability),
                                "reasons": format!("{:?}", candidate.reasons),
                            })
                        })
                        .collect();
                    json!({"kind": "decision", "selection": format!("{:?}", decision.selection), "candidates": candidates})
                }
                Event::Preflight {
                    route,
                    video_bps,
                    result,
                    detail,
                } => json!({
                    "kind": "preflight", "encoder": route.encoder(codec), "video_bps": video_bps,
                    "result": format!("{result:?}"), "detail": detail
                }),
                Event::Encoding { route, video_bps } => json!({
                    "kind": "encoding", "encoder": route.encoder(codec), "video_bps": video_bps
                }),
                Event::Validating => json!({"kind": "validating"}),
                Event::RouteFailed { route, detail } => json!({
                    "kind": "route_failed", "encoder": route.encoder(codec), "detail": detail
                }),
                Event::Oversize {
                    measured,
                    next_video_bps,
                } => json!({
                    "kind": "oversize", "measured_bytes": measured, "next_video_bps": next_video_bps
                }),
            };
            events.push(json!({"elapsed_ms": start.elapsed().as_millis(), "event": event}));
        },
    );
    let elapsed_ms = start.elapsed().as_millis();
    let unchanged = input
        .iter()
        .zip(&fingerprints)
        .all(|(path, expected)| hash(path).is_ok_and(|actual| &actual == expected));
    let success = result.is_ok() && unchanged;
    let output = match result {
        Ok(out) => json!({
            "published": true, "bytes": out.bytes, "encoder": out.route.encoder(codec),
            "selection": format!("{:?}", out.selection), "video_bps": out.video_bps,
            "eta": out.production_eta.map(|e| json!({
                "lower_ms": e.total.lower.as_millis(), "upper_ms": e.total.upper.as_millis(),
            })),
        }),
        Err(error) => json!({"published": false, "error": format!("{error:?}")}),
    };
    let report = json!({
        "schema": 1,
        "input_unchanged": unchanged,
        "source_sha256": fingerprints[0],
        "ffmpeg_sha256": fingerprints[1],
        "ffprobe_sha256": fingerprints[2],
        "cpu": inventory.cpu,
        "logical_cpus": inventory.logical_cpus,
        "adapters": adapters,
        "adapter_scan_complete": inventory.adapter_scan_complete,
        "codec": format!("{codec:?}"),
        "limit_bytes": limit_bytes,
        "max_long_edge": max_long_edge,
        "profile": input_profile,
        "events": events,
        "core_elapsed_ms": elapsed_ms,
        "output": output,
    });
    let mut stdout = BufWriter::new(io::stdout().lock());
    serde_json::to_writer_pretty(&mut stdout, &report)?;
    writeln!(stdout)?;
    stdout.flush()?;
    Ok(success)
}

fn main() -> std::process::ExitCode {
    match run() {
        Ok(true) => std::process::ExitCode::SUCCESS,
        Ok(false) => std::process::ExitCode::from(1),
        Err(error) => {
            eprintln!("{error}");
            std::process::ExitCode::from(2)
        }
    }
}
