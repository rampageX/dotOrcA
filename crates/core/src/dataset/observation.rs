use crate::{
    budget::Plan,
    decision::{FallbackReason, Route, Selection},
    hardware::Inventory,
    media::{Hdr, MediaInfo},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{fs::File, io, path::Path, time::Duration};

fn label(s: &str) -> bool {
    !s.is_empty() && s.len() <= 256 && !s.chars().any(char::is_control)
}

fn digest(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

const ENCODERS: &[&str] = &[
    "h264_nvenc",
    "hevc_nvenc",
    "av1_nvenc",
    "h264_qsv",
    "hevc_qsv",
    "av1_qsv",
    "h264_amf",
    "hevc_amf",
    "av1_amf",
    "libx264",
    "libx265",
    "libsvtav1",
];

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Fingerprint {
    ffmpeg_sha256: String,
    ffprobe_sha256: String,
}

impl Fingerprint {
    pub(crate) fn read(ffmpeg: &Path, ffprobe: &Path) -> io::Result<Self> {
        fn hash(path: &Path) -> io::Result<String> {
            let mut hash = Sha256::new();
            io::copy(&mut File::open(path)?, &mut hash)?;
            Ok(format!("{:x}", hash.finalize()))
        }
        Ok(Self {
            ffmpeg_sha256: hash(ffmpeg)?,
            ffprobe_sha256: hash(ffprobe)?,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Adapter {
    name: Option<String>,
    driver: Option<String>,
    instance: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    os: String,
    arch: String,
    cpu: Option<String>,
    threads: Option<usize>,
    adapters_complete: bool,
    adapters: Vec<Adapter>,
    tools: Fingerprint,
    encoder: String,
    width: u64,
    height: u64,
    depth: u64,
    video_bps: u64,
    audio_bps: Vec<u64>,
    limit_bytes: u64,
    duration_us: u64,
    source_width: u64,
    source_height: u64,
    source_codec: Option<String>,
    pixel_format: Option<String>,
    rate: Option<(u64, u64)>,
    nominal_rate: Option<(u64, u64)>,
    hdr: String,
    colors: [Option<String>; 4],
    audio: Vec<(Option<String>, Option<u64>, Option<u64>)>,
}

impl Profile {
    /// Build a comparable key from a probed source, its S04 plan, and the
    /// proposed S06 route and bitrate. Reads only the encoding tools.
    pub fn capture(
        media: &MediaInfo,
        plan: &Plan<'_>,
        inventory: &Inventory,
        route: Route,
        video_bps: u64,
        ffmpeg: &Path,
        ffprobe: &Path,
    ) -> io::Result<Self> {
        let tools = Fingerprint::read(ffmpeg, ffprobe)?;
        Ok(Self::new(media, plan, inventory, route, video_bps, tools))
    }

    pub(crate) fn new(
        media: &MediaInfo,
        plan: &Plan<'_>,
        inventory: &Inventory,
        route: Route,
        video_bps: u64,
        tools: Fingerprint,
    ) -> Self {
        let mut adapters: Vec<_> = inventory
            .adapters
            .iter()
            .map(|a| Adapter {
                name: a.name.clone(),
                driver: a.driver_version.clone(),
                instance: a.instance_id.clone(),
            })
            .collect();
        adapters.sort();
        let src = plan.source;
        Self {
            os: std::env::consts::OS.into(),
            arch: std::env::consts::ARCH.into(),
            cpu: inventory.cpu.clone(),
            threads: inventory.logical_cpus,
            adapters_complete: inventory.adapter_scan_complete,
            adapters,
            tools,
            encoder: route.encoder(plan.codec).into(),
            width: plan.width,
            height: plan.height,
            depth: src.bit_depth.unwrap_or_default(),
            video_bps,
            audio_bps: plan.audio_bps.clone(),
            limit_bytes: plan.limit_bytes,
            duration_us: plan.duration_us,
            source_width: src.width.unwrap_or_default(),
            source_height: src.height.unwrap_or_default(),
            source_codec: src.codec.clone(),
            pixel_format: src.pixel_format.clone(),
            rate: src.avg_rate.map(|r| (r.numerator, r.denominator)),
            nominal_rate: src.nominal_rate.map(|r| (r.numerator, r.denominator)),
            hdr: match src.hdr {
                Hdr::Sdr => "sdr",
                Hdr::Pq => "pq",
                Hdr::Hlg => "hlg",
                Hdr::DolbyVision => "dolby-vision",
                Hdr::Unknown => "unknown",
            }
            .into(),
            colors: [
                src.color_range.clone(),
                src.color_space.clone(),
                src.color_transfer.clone(),
                src.color_primaries.clone(),
            ],
            audio: media
                .streams
                .iter()
                .filter(|s| s.kind.as_deref() == Some("audio"))
                .map(|s| (s.codec.clone(), s.channels, s.sample_rate))
                .collect(),
        }
    }

    fn valid(&self) -> bool {
        label(&self.os)
            && label(&self.arch)
            && digest(&self.tools.ffmpeg_sha256)
            && digest(&self.tools.ffprobe_sha256)
            && ENCODERS.contains(&self.encoder.as_str())
            && self.width >= 2
            && self.width <= self.source_width
            && self.height >= 2
            && self.height <= self.source_height
            && matches!(self.depth, 8 | 10)
            && self.video_bps > 0
            && self.limit_bytes > 0
            && self.duration_us > 0
            && matches!(self.hdr.as_str(), "sdr" | "pq" | "hlg" | "unknown")
            && self.audio.len() == self.audio_bps.len()
            && self.audio_bps.iter().all(|r| *r > 0)
            && [self.rate, self.nominal_rate]
                .into_iter()
                .flatten()
                .all(|(n, d)| n > 0 && d > 0)
            && [&self.cpu, &self.source_codec, &self.pixel_format]
                .into_iter()
                .chain(&self.colors)
                .chain(self.audio.iter().map(|a| &a.0))
                .chain(
                    self.adapters
                        .iter()
                        .flat_map(|a| [&a.name, &a.driver, &a.instance]),
                )
                .filter_map(Option::as_deref)
                .all(label)
    }

    pub(crate) fn complete(&self) -> bool {
        self.valid()
            && self.cpu.is_some()
            && self.threads.is_some_and(|n| n > 0)
            && self.adapters_complete
            && self
                .adapters
                .iter()
                .all(|a| a.name.is_some() && a.driver.is_some() && a.instance.is_some())
            && (self.encoder.starts_with("lib") || !self.adapters.is_empty())
            && self.source_codec.is_some()
            && self.pixel_format.is_some()
            && self.rate.is_some()
            && self.hdr != "unknown"
            && self.audio.iter().all(|(codec, channels, rate)| {
                codec.is_some() && channels.is_some_and(|n| n > 0) && rate.is_some_and(|n| n > 0)
            })
    }
}

pub(crate) struct Outcome {
    pub total: Duration,
    pub encode: Duration,
    pub bytes: u64,
    pub retries: usize,
    pub failed_preflights: Vec<&'static str>,
    pub failed_encodes: Vec<&'static str>,
    pub selection: Selection,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Observation {
    source: String,
    profile: Profile,
    total_us: u64,
    encode_us: u64,
    bytes: u64,
    capacity_retries: usize,
    failed_preflights: Vec<String>,
    failed_encodes: Vec<String>,
    selection: String,
}

impl Observation {
    pub(crate) fn published(profile: Profile, result: Outcome) -> Self {
        let micros = |d: Duration| u64::try_from(d.as_micros()).unwrap_or(u64::MAX).max(1);
        Self {
            source: "s06-verified-publication-v1".into(),
            profile,
            total_us: micros(result.total),
            encode_us: micros(result.encode),
            bytes: result.bytes,
            capacity_retries: result.retries,
            failed_preflights: result
                .failed_preflights
                .into_iter()
                .map(str::to_owned)
                .collect(),
            failed_encodes: result
                .failed_encodes
                .into_iter()
                .map(str::to_owned)
                .collect(),
            selection: match result.selection {
                Selection::DefaultHardware(_) => "hardware",
                Selection::SoftwareFallback(FallbackReason::HardwareUnavailable) => {
                    "hardware-unavailable"
                }
                Selection::SoftwareFallback(FallbackReason::HardwareExecutionFailed) => {
                    "hardware-execution-failed"
                }
                Selection::ProactiveSoftware => "proactive-software",
                Selection::NoRoute => "no-route",
            }
            .into(),
        }
    }

    pub fn profile(&self) -> &Profile {
        &self.profile
    }

    pub(crate) fn valid(&self) -> bool {
        let software = self.profile.encoder.starts_with("lib");
        self.source == "s06-verified-publication-v1"
            && self.profile.valid()
            && self.encode_us > 0
            && self.total_us >= self.encode_us
            && (1..=self.profile.limit_bytes).contains(&self.bytes)
            && self.capacity_retries <= 2
            && self.failed_preflights.len() + self.failed_encodes.len() <= 3
            && self
                .failed_preflights
                .iter()
                .chain(&self.failed_encodes)
                .all(|e| ENCODERS.contains(&e.as_str()) && e != &self.profile.encoder)
            && match self.selection.as_str() {
                "hardware" => !software,
                "hardware-unavailable" | "proactive-software" => software,
                "hardware-execution-failed" => software && !self.failed_encodes.is_empty(),
                _ => false,
            }
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct History {
    pub samples: usize,
    pub slowest_observed_total: Duration,
    pub largest_observed_bytes: u64,
}

impl History {
    pub(crate) fn include(&mut self, sample: &Observation) {
        self.samples += 1;
        self.slowest_observed_total = self
            .slowest_observed_total
            .max(Duration::from_micros(sample.total_us));
        self.largest_observed_bytes = self.largest_observed_bytes.max(sample.bytes);
    }
}
