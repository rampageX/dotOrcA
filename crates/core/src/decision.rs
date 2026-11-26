//! S05 route selection. S06 owns execution and final output validation.
use crate::{
    budget::{self, Codec, Plan},
    hardware::{self, Evidence},
    media::Hdr,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    Nvenc,
    Qsv,
    Amf,
    Software,
}

impl Route {
    pub fn encoder(self, codec: Codec) -> &'static str {
        match (self, codec) {
            (Self::Nvenc, Codec::H264) => "h264_nvenc",
            (Self::Nvenc, Codec::Hevc) => "hevc_nvenc",
            (Self::Nvenc, Codec::Av1) => "av1_nvenc",
            (Self::Qsv, Codec::H264) => "h264_qsv",
            (Self::Qsv, Codec::Hevc) => "hevc_qsv",
            (Self::Qsv, Codec::Av1) => "av1_qsv",
            (Self::Amf, Codec::H264) => "h264_amf",
            (Self::Amf, Codec::Hevc) => "hevc_amf",
            (Self::Amf, Codec::Av1) => "av1_amf",
            (Self::Software, Codec::H264) => "libx264",
            (Self::Software, Codec::Hevc) => "libx265",
            (Self::Software, Codec::Av1) => "libsvtav1",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    UnknownBitDepth,
    UnsupportedBitDepth,
    HdrBitDepthConflict,
    UnknownPixelFormat,
    UnknownSourceRate,
    UnknownHdr,
    IncompleteFingerprint,
    SmallProbeOnly,
    ProbeMissing,
    ProbeNotListed,
    AdapterAbsent,
    ProbeFailed,
    ProbeTimedOut,
    RequestUnverified,
    RequestVerified,
    RequestRejected,
    ExecutionFailed,
    ConflictingRouteEvidence,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Capability {
    Pending,
    Ready,
    Unavailable,
}

#[derive(Debug)]
pub struct Candidate<'a> {
    pub route: Route,
    pub encoder: &'static str,
    pub capability: Capability,
    pub reasons: Vec<Reason>,
    pub small_probe: Option<&'a hardware::Capability>,
    pub request_checks: Vec<RequestCheck<'a>>,
}

/// An in-memory planning attempt. Evidence is scoped to the very same S04 plan
/// and S01 snapshot. Refresh either input after source/tool/driver changes.
#[derive(Clone, Copy, Debug)]
pub struct Job<'a> {
    pub plan: &'a Plan<'a>,
    pub hardware: &'a hardware::Report,
}

impl Job<'_> {
    fn matches(self, other: Self) -> bool {
        std::ptr::eq(self.plan, other.plan) && std::ptr::eq(self.hardware, other.hardware)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckResult {
    /// Preflight covers the actual encoder, dimensions, depth, rate-control,
    /// timestamp handling and source color/metadata requirements for this plan.
    Compatible,
    Incompatible,
    /// S06 reported an encoder-route failure. Cancellation, disk capacity
    /// and other task-wide failures do not belong here.
    ExecutionFailed,
}

#[derive(Clone, Copy, Debug)]
pub struct RequestCheck<'a> {
    pub job: Job<'a>,
    pub route: Route,
    pub result: CheckResult,
    pub evidence_id: &'a str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FallbackReason {
    HardwareUnavailable,
    HardwareExecutionFailed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Selection {
    DefaultHardware(Route),
    SoftwareFallback(FallbackReason),
    NoRoute,
}

#[derive(Debug)]
pub struct Decision<'a> {
    pub job: Job<'a>,
    pub selection: Selection,
    pub candidates: Vec<Candidate<'a>>,
    pub constraints: Vec<Reason>,
    pub unknowns: Vec<Reason>,
}

impl Decision<'_> {
    pub fn ready_route(&self) -> Option<Route> {
        let route = match self.selection {
            Selection::DefaultHardware(route) => route,
            Selection::SoftwareFallback(_) => Route::Software,
            Selection::NoRoute => return None,
        };
        self.candidates
            .iter()
            .any(|c| c.route == route && c.capability == Capability::Ready)
            .then_some(route)
    }
}

fn candidate<'a>(job: Job<'a>, route: Route, checks: &[RequestCheck<'a>]) -> Candidate<'a> {
    let encoder = route.encoder(job.plan.codec);
    let small_probe = job.hardware.capabilities.iter().find(|probe| {
        route != Route::Software
            && probe.encoder == encoder
            && Some(u64::from(probe.bit_depth)) == job.plan.source.bit_depth
    });
    let request_checks: Vec<_> = checks
        .iter()
        .copied()
        .filter(|check| {
            job.matches(check.job) && check.route == route && !check.evidence_id.trim().is_empty()
        })
        .collect();

    let mut reasons = Vec::new();
    let probe_result = match small_probe.map(|probe| &probe.evidence) {
        Some(Evidence::Verified) => Some((Capability::Pending, Reason::SmallProbeOnly)),
        Some(Evidence::NotListed) => Some((Capability::Unavailable, Reason::ProbeNotListed)),
        Some(Evidence::AdapterAbsent) => Some((Capability::Unavailable, Reason::AdapterAbsent)),
        Some(Evidence::Failed) => Some((Capability::Unavailable, Reason::ProbeFailed)),
        Some(Evidence::TimedOut) => Some((Capability::Unavailable, Reason::ProbeTimedOut)),
        None if route != Route::Software => Some((Capability::Pending, Reason::ProbeMissing)),
        None => None,
    };
    reasons.extend(probe_result.map(|(_, reason)| reason));

    let has = |result| request_checks.iter().any(|check| check.result == result);
    let (capability, reason) = if has(CheckResult::ExecutionFailed) {
        (Capability::Unavailable, Reason::ExecutionFailed)
    } else if has(CheckResult::Incompatible) && has(CheckResult::Compatible) {
        (Capability::Unavailable, Reason::ConflictingRouteEvidence)
    } else if has(CheckResult::Incompatible) {
        (Capability::Unavailable, Reason::RequestRejected)
    } else if probe_result.is_some_and(|(state, reason)| {
        state == Capability::Unavailable && reason == Reason::ProbeNotListed
    }) && has(CheckResult::Compatible)
    {
        (Capability::Unavailable, Reason::ConflictingRouteEvidence)
    } else if has(CheckResult::Compatible) {
        (Capability::Ready, Reason::RequestVerified)
    } else {
        (
            probe_result.map_or(Capability::Pending, |(state, _)| state),
            Reason::RequestUnverified,
        )
    };
    reasons.push(reason);
    Candidate {
        route,
        encoder,
        capability,
        reasons,
        small_probe,
        request_checks,
    }
}

/// Pending candidates require request preflight. A pending selection is an
/// ordering decision and never authorizes execution or a preservation claim.
pub fn decide<'a>(
    budget: Result<&'a Plan<'a>, budget::Error>,
    hardware: &'a hardware::Report,
    checks: &[RequestCheck<'a>],
) -> Result<Decision<'a>, budget::Error> {
    let plan = budget?;
    let job = Job { plan, hardware };
    let source = plan.source;
    let mut constraints = Vec::new();
    let mut unknowns = Vec::new();
    match source.bit_depth {
        None => constraints.push(Reason::UnknownBitDepth),
        Some(8) if matches!(source.hdr, Hdr::Pq | Hdr::Hlg | Hdr::DolbyVision) => {
            constraints.push(Reason::HdrBitDepthConflict);
        }
        Some(8 | 10) => {}
        Some(_) => constraints.push(Reason::UnsupportedBitDepth),
    }
    if source.bit_depth.is_none() {
        unknowns.push(Reason::UnknownBitDepth);
    }
    if source.pixel_format.is_none() {
        unknowns.push(Reason::UnknownPixelFormat);
    }
    if plan.source_rate.is_none() {
        unknowns.push(Reason::UnknownSourceRate);
    }
    if source.hdr == Hdr::Unknown {
        unknowns.push(Reason::UnknownHdr);
    }
    let inv = &hardware.inventory;
    if hardware.tool.modified.is_none()
        || hardware.tool_version.trim().is_empty()
        || inv.cpu.is_none()
        || inv.logical_cpus.is_none()
        || !inv.adapter_scan_complete
        || inv
            .adapters
            .iter()
            .any(|a| a.instance_id.is_none() || a.driver_version.is_none())
    {
        unknowns.push(Reason::IncompleteFingerprint);
    }

    let mut candidates: Vec<_> = [Route::Nvenc, Route::Qsv, Route::Amf, Route::Software]
        .into_iter()
        .map(|route| candidate(job, route, checks))
        .collect();
    if !constraints.is_empty() {
        for entry in &mut candidates {
            entry.capability = Capability::Unavailable;
            entry.reasons.extend(constraints.iter().copied());
        }
    }
    for reason in [Reason::ProbeMissing, Reason::RequestUnverified] {
        if candidates
            .iter()
            .any(|c| c.capability == Capability::Pending && c.reasons.contains(&reason))
        {
            unknowns.push(reason);
        }
    }

    let (software, hardware_candidates) = candidates.split_last().expect("software candidate");
    let hw = hardware_candidates
        .iter()
        .filter(|c| c.capability != Capability::Unavailable)
        .min_by_key(|c| match (c.capability, c.small_probe.is_some()) {
            (Capability::Ready, _) => 0,
            (Capability::Pending, true) => 1,
            _ => 2,
        });
    // CPU-only CI explicitly enables software encoding for fixture tests.
    let allow_fallback = false;
    #[cfg(test)]
    let allow_fallback = allow_fallback || FIXTURE_FALLBACK.get();
    let selection = match hw {
        Some(hardware) => Selection::DefaultHardware(hardware.route),
        None if software.capability != Capability::Unavailable && allow_fallback => {
            let reason = if hardware_candidates
                .iter()
                .any(|c| c.reasons.contains(&Reason::ExecutionFailed))
            {
                FallbackReason::HardwareExecutionFailed
            } else {
                FallbackReason::HardwareUnavailable
            };
            Selection::SoftwareFallback(reason)
        }
        None => Selection::NoRoute,
    };
    Ok(Decision {
        job,
        selection,
        candidates,
        constraints,
        unknowns,
    })
}

#[cfg(test)]
thread_local! {
    static FIXTURE_FALLBACK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
pub(crate) fn fixture_fallback() -> bool {
    FIXTURE_FALLBACK.get()
}

#[cfg(test)]
pub(crate) fn allow_fixture_fallback() {
    FIXTURE_FALLBACK.set(true);
}

#[cfg(test)]
mod tests;
