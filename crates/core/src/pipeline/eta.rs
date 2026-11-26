//! Production ETA inputs. Calibration and admission of measured coefficients
//! belong to S09. S06 publishes unknown ETA.
use crate::{
    budget::Codec,
    decision::Route,
    hardware::{Inventory, ToolFingerprint},
};
use std::{
    path::Path,
    time::{Duration, SystemTime},
};

#[derive(Debug, PartialEq, Eq)]
pub struct Profile<'a> {
    pub source: &'a Path,
    pub source_size_bytes: u64,
    pub source_modified: SystemTime,
    pub source_duration_us: u64,
    pub video_encoder: Route,
    pub codec: Codec,
    pub width: u64,
    pub height: u64,
    pub depth: u64,
    pub video_bps: u64,
    pub audio_bps: &'a [u64],
    pub ffmpeg: &'a ToolFingerprint,
    pub ffprobe: &'a ToolFingerprint,
    pub inventory: &'a Inventory,
}

#[derive(Clone, Copy, Debug)]
pub struct Bounds {
    pub lower: Duration,
    pub upper: Duration,
}

#[derive(Clone, Copy, Debug)]
pub struct Estimate {
    pub total: Bounds,
}

#[derive(Debug)]
pub struct Calibrated<'a> {
    pub profile: Profile<'a>,
    pub evidence_id: &'a str,
    /// Process initialization, encoder initialization and final mux close.
    pub fixed: Bounds,
    /// Wall time per second of source, including decode, scale, video encode,
    /// audio and mux throughput in the actual production configuration.
    pub sustained_per_source_second: Bounds,
    /// Preflight and source/output validation, measured for this source
    /// duration. These stages execute outside the sustained encoding pass.
    pub verification: Bounds,
}

pub fn estimate(profile: &Profile<'_>, evidence: Option<&Calibrated<'_>>) -> Option<Estimate> {
    let evidence = evidence?;
    if &evidence.profile != profile
        || profile.source_duration_us == 0
        || evidence.evidence_id.trim().is_empty()
        || evidence.sustained_per_source_second.lower.is_zero()
        || [
            evidence.fixed,
            evidence.verification,
            evidence.sustained_per_source_second,
        ]
        .iter()
        .any(|b| b.lower > b.upper)
    {
        return None;
    }
    let total = |fixed: Duration, sustained: Duration, verification: Duration, upper| {
        let nanos = sustained
            .as_nanos()
            .checked_mul(u128::from(profile.source_duration_us))?;
        let scaled = if upper {
            nanos.div_ceil(1_000_000)
        } else {
            nanos / 1_000_000
        };
        let continuous = Duration::new(
            u64::try_from(scaled / 1_000_000_000).ok()?,
            (scaled % 1_000_000_000) as u32,
        );
        fixed.checked_add(continuous)?.checked_add(verification)
    };
    Some(Estimate {
        total: Bounds {
            lower: total(
                evidence.fixed.lower,
                evidence.sustained_per_source_second.lower,
                evidence.verification.lower,
                false,
            )?,
            upper: total(
                evidence.fixed.upper,
                evidence.sustained_per_source_second.upper,
                evidence.verification.upper,
                true,
            )?,
        },
    })
}
