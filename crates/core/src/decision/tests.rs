use super::*;
use crate::media::{self, MediaInfo};
use serde_json::json;

fn source(pixel_format: &str, transfer: &str) -> MediaInfo {
    let probe = json!({
        "format": {"duration": "180"},
        "streams": [{"codec_type": "video", "width": 3840, "height": 2160,
            "avg_frame_rate": "24000/1001", "pix_fmt": pixel_format, "color_transfer": transfer,
            "sample_aspect_ratio": "1:1"}]
    });
    media::parse(
        "小K 的鹅腿坟场 & (黑洞) ^ % !.avi".into(),
        probe.to_string().as_bytes(),
    )
    .unwrap()
}

fn report(nvenc: Evidence, depth: u8) -> hardware::Report {
    hardware::Report {
        inventory: hardware::Inventory {
            cpu: Some("test CPU".into()),
            logical_cpus: Some(8),
            adapters: vec![hardware::Adapter {
                name: Some("test GPU".into()),
                instance_id: Some("test device".into()),
                driver_version: Some("test driver".into()),
            }],
            adapter_scan_complete: true,
            adapter_scan_error: None,
        },
        tool_version: "test FFmpeg".into(),
        tool: hardware::ToolFingerprint {
            executable: "test-ffmpeg".into(),
            size_bytes: 100,
            modified: Some(std::time::UNIX_EPOCH),
        },
        capabilities: [
            (Route::Nvenc, nvenc),
            (Route::Qsv, Evidence::NotListed),
            (Route::Amf, Evidence::Failed),
        ]
        .into_iter()
        .map(|(route, evidence)| hardware::Capability {
            encoder: route.encoder(Codec::Hevc).into(),
            bit_depth: depth,
            evidence,
            diagnostic: "test small probe".into(),
        })
        .collect(),
        cache_hit: false,
    }
}

fn check(job: Job<'_>, route: Route, result: CheckResult) -> RequestCheck<'_> {
    RequestCheck {
        job,
        route,
        result,
        evidence_id: "test request preflight",
    }
}

fn calibrated<T>(value: T) -> Option<Observation<'static, T>> {
    Some(Observation {
        origin: Origin::CalibratedProduction,
        evidence_id: "test calibration",
        value,
    })
}

fn qualifying(job: Job<'_>) -> SoftwareEvidence<'_> {
    SoftwareEvidence {
        job,
        compared_with: Route::Nvenc,
        eta_upper_us: calibrated(180_000_000),
        speed_lower: calibrated(Rational {
            numerator: 1,
            denominator: 1,
        }),
        benefit: calibrated(Benefit::QualityGainBps(1)),
    }
}

#[test]
fn proactive_software_boundary_and_each_independent_denial() {
    let media = source("yuv420p", "bt709");
    let plan = budget::plan(&media, 250_000_000, Codec::Hevc, 1920).unwrap();
    let hardware = report(Evidence::Verified, 8);
    let job = Job {
        plan: &plan,
        hardware: &hardware,
    };
    let checks = [Route::Nvenc, Route::Software].map(|r| check(job, r, CheckResult::Compatible));
    let good = qualifying(job);
    let run = |e| decide(Ok(&plan), &hardware, &checks, Some(e)).unwrap();
    assert_eq!(run(good).selection, Selection::ProactiveSoftware);
    assert_eq!(run(good).ready_route(), Some(Route::Software));
    assert_eq!(
        run(SoftwareEvidence {
            benefit: calibrated(Benefit::CapacitySavingBps(1)),
            ..good
        })
        .selection,
        Selection::ProactiveSoftware
    );

    for (evidence, denial) in [
        (
            SoftwareEvidence {
                eta_upper_us: calibrated(180_000_001),
                ..good
            },
            Reason::EtaTooLong,
        ),
        (
            SoftwareEvidence {
                eta_upper_us: None,
                ..good
            },
            Reason::EtaUnknown,
        ),
        (
            SoftwareEvidence {
                speed_lower: calibrated(Rational {
                    numerator: 999,
                    denominator: 1000,
                }),
                ..good
            },
            Reason::SpeedBelowRealtime,
        ),
        (
            SoftwareEvidence {
                speed_lower: None,
                ..good
            },
            Reason::SpeedUnknown,
        ),
        (
            SoftwareEvidence {
                benefit: calibrated(Benefit::QualityGainBps(0)),
                ..good
            },
            Reason::BenefitNotPositive,
        ),
        (
            SoftwareEvidence {
                benefit: calibrated(Benefit::CapacitySavingBps(-1)),
                ..good
            },
            Reason::BenefitNotPositive,
        ),
        (
            SoftwareEvidence {
                benefit: None,
                ..good
            },
            Reason::BenefitUnknown,
        ),
    ] {
        let result = run(evidence);
        assert_eq!(result.selection, Selection::DefaultHardware(Route::Nvenc));
        assert_eq!(result.proactive_denials, [denial]);
    }
}

#[test]
fn references_and_stale_evidence_cannot_authorize_software() {
    let media = source("yuv420p", "bt709");
    let plan = budget::plan(&media, 250_000_000, Codec::Hevc, 1920).unwrap();
    let hardware = report(Evidence::Verified, 8);
    let job = Job {
        plan: &plan,
        hardware: &hardware,
    };
    let checks = [Route::Nvenc, Route::Software].map(|r| check(job, r, CheckResult::Compatible));
    let mut weak = qualifying(job);
    weak.eta_upper_us.as_mut().unwrap().origin = Origin::S03ShortWindow;
    weak.speed_lower.as_mut().unwrap().origin = Origin::S03Reference;
    weak.benefit.as_mut().unwrap().origin = Origin::S02Synthetic;
    let mut changed = hardware.clone();
    changed.inventory.adapters[0].driver_version = Some("new driver".into());
    let current = Job {
        plan: &plan,
        hardware: &changed,
    };
    let current_checks =
        [Route::Nvenc, Route::Software].map(|r| check(current, r, CheckResult::Compatible));

    for result in [
        decide(Ok(&plan), &hardware, &checks, Some(weak)).unwrap(),
        decide(Ok(&plan), &changed, &current_checks, Some(qualifying(job))).unwrap(),
    ] {
        assert_eq!(result.selection, Selection::DefaultHardware(Route::Nvenc));
        assert_eq!(
            result.proactive_denials,
            [
                Reason::EtaUntrusted,
                Reason::SpeedUntrusted,
                Reason::BenefitUntrusted
            ]
        );
    }
    let stale = decide(Ok(&plan), &changed, &checks, None).unwrap();
    assert_eq!(stale.ready_route(), None);
    assert!(stale.candidates[0].request_checks.is_empty());
}

#[test]
fn small_probe_only_sets_order_and_exact_request_checks_preserve_constraints() {
    let media = source("yuv420p10le", "smpte2084");
    let hardware = report(Evidence::Verified, 10);
    let plan = budget::plan(&media, 500_000_000, Codec::Hevc, 1920).unwrap();
    let job = Job {
        plan: &plan,
        hardware: &hardware,
    };
    let d = decide(Ok(&plan), &hardware, &[], None).unwrap();
    assert_eq!(d.selection, Selection::DefaultHardware(Route::Nvenc));
    assert_eq!(d.ready_route(), None);
    assert_eq!(
        d.candidates[0].reasons,
        [Reason::SmallProbeOnly, Reason::RequestUnverified]
    );
    assert_eq!((d.job.plan.width, d.job.plan.height), (1920, 1080));
    assert_eq!(
        d.job.plan.source_rate,
        Some(Rational {
            numerator: 24000,
            denominator: 1001
        })
    );
    assert_eq!(d.job.plan.source.hdr, Hdr::Pq);
    assert_eq!(d.job.plan.source.bit_depth, Some(10));
    assert!(d.unknowns.contains(&Reason::EtaUnknown));

    let checks = [
        check(job, Route::Nvenc, CheckResult::Incompatible),
        check(job, Route::Software, CheckResult::Compatible),
    ];
    let d = decide(Ok(&plan), &hardware, &checks, None).unwrap();
    assert_eq!(d.selection, Selection::NoRoute);
    assert_eq!(d.ready_route(), None);
    assert_eq!(d.candidates[3].encoder, "libx265");
    assert_eq!(
        d.job.plan.source.color_transfer.as_deref(),
        Some("smpte2084")
    );
}

#[test]
fn unavailable_and_failed_hardware_do_not_fall_back_to_software() {
    let media = source("yuv420p", "bt709");
    let plan = budget::plan(&media, 200_000_000, Codec::Hevc, 1920).unwrap();
    let unavailable = report(Evidence::NotListed, 8);
    let d = decide(Ok(&plan), &unavailable, &[], None).unwrap();
    assert_eq!(d.selection, Selection::NoRoute);
    assert_eq!(d.ready_route(), None);

    let mut hardware = report(Evidence::Verified, 8);
    hardware.capabilities[1].evidence = Evidence::Verified;
    let job = Job {
        plan: &plan,
        hardware: &hardware,
    };
    let checks = [
        check(job, Route::Nvenc, CheckResult::Compatible),
        check(job, Route::Nvenc, CheckResult::ExecutionFailed),
        check(job, Route::Qsv, CheckResult::Compatible),
        check(job, Route::Software, CheckResult::Compatible),
    ];
    let d = decide(Ok(&plan), &hardware, &checks, None).unwrap();
    assert_eq!(d.selection, Selection::DefaultHardware(Route::Qsv));
    assert_eq!(d.ready_route(), Some(Route::Qsv));

    let mut all_failed = checks.to_vec();
    all_failed.push(check(job, Route::Qsv, CheckResult::ExecutionFailed));
    let d = decide(Ok(&plan), &hardware, &all_failed, None).unwrap();
    assert_eq!(d.selection, Selection::NoRoute);
    assert_eq!(d.ready_route(), None);
    all_failed.push(check(job, Route::Software, CheckResult::Incompatible));
    let d = decide(Ok(&plan), &hardware, &all_failed, None).unwrap();
    assert_eq!(d.selection, Selection::NoRoute);
}

#[test]
fn original_budget_error_and_conflicting_format_are_preserved() {
    let hardware = report(Evidence::Verified, 8);
    assert_eq!(
        decide(Err(budget::Error::UnknownDuration), &hardware, &[], None).unwrap_err(),
        budget::Error::UnknownDuration
    );

    for (pixel_format, expected) in [
        ("yuv420p", Reason::HdrBitDepthConflict),
        ("yuv420p12le", Reason::UnsupportedBitDepth),
        ("unknown", Reason::UnknownBitDepth),
    ] {
        let media = source(pixel_format, "arib-std-b67");
        let plan = budget::plan(&media, 200_000_000, Codec::Av1, 1920).unwrap();
        let d = decide(Ok(&plan), &hardware, &[], None).unwrap();
        assert_eq!(d.selection, Selection::NoRoute);
        assert_eq!(d.constraints, [expected]);
        assert_eq!(d.candidates[0].encoder, "av1_nvenc");
        assert_eq!(d.candidates[3].encoder, "libsvtav1");
        assert_eq!(d.job.plan.source.hdr, Hdr::Hlg);
    }
}

#[test]
fn probe_evidence_is_codec_and_depth_specific() {
    let media = source("yuv420p10le", "bt709");
    let plan = budget::plan(&media, 250_000_000, Codec::Hevc, 1920).unwrap();
    let hardware = report(Evidence::Verified, 8);
    let d = decide(Ok(&plan), &hardware, &[], None).unwrap();
    assert!(d
        .candidates
        .iter()
        .all(|candidate| candidate.small_probe.is_none()));
    assert_eq!(d.ready_route(), None);
    assert!(d.unknowns.contains(&Reason::ProbeMissing));
}
