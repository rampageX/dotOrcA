use super::*;
use crate::{
    budget::{self, Codec},
    decision::{FallbackReason, Route, Selection},
    hardware::Inventory,
    media,
};
use std::{
    fs,
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::Duration,
};

const LEGACY: &str = include_str!("../../../../data/official/s02-physical-v1.json");

struct Root(PathBuf);

impl Root {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "dotorca-s07-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for Root {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn package(version: u64, schema: u32, dataset: &str) -> (String, String) {
    let p = serde_json::to_string(&Release {
        schema,
        version,
        dataset: dataset.into(),
    })
    .unwrap();
    let digest = format!("{:x}", Sha256::digest(p.as_bytes()));
    (p, digest)
}

fn sample(root: &Path, cap: u64, bytes: u64, micros: u64) -> Observation {
    let exe = root.join("tool");
    let media = media::parse(
        root.join("private source.mp4"),
        br#"{
        "format":{"duration":"1.5"},
        "streams":[{"codec_type":"video","codec_name":"h264","width":320,"height":180,
        "pix_fmt":"yuv420p","avg_frame_rate":"30/1","r_frame_rate":"30/1","color_transfer":"bt709"}]
    }"#,
    )
    .unwrap();
    let plan = budget::plan(&media, cap, Codec::H264, 320).unwrap();
    let inventory = Inventory {
        cpu: Some("Test CPU".into()),
        logical_cpus: Some(8),
        adapters: Vec::new(),
        adapter_scan_complete: true,
        adapter_scan_error: None,
    };
    let profile = Profile::capture(
        &media,
        &plan,
        &inventory,
        Route::Software,
        plan.video_bps,
        &exe,
        &exe,
    )
    .unwrap();
    Observation::published(
        profile,
        Outcome {
            total: Duration::from_micros(micros),
            encode: Duration::from_micros(3),
            bytes,
            retries: 0,
            failed_encodes: Vec::new(),
            failed_preflights: Vec::new(),
            selection: Selection::SoftwareFallback(FallbackReason::HardwareUnavailable),
        },
    )
}

#[test]
fn official_update_validation_failure_and_interruption() {
    let root = Root::new();
    let store = Store::portable(&root.0).unwrap();
    let (version, old) = store.official().unwrap();
    assert_eq!(version, 1);
    assert_eq!(old.info()["official_samples"], 264);
    let (p2, h2) = package(2, SCHEMA, LEGACY);
    for (p, h) in [
        package(2, 99, LEGACY),
        package(2, SCHEMA, "[]"),
        package(1, SCHEMA, LEGACY),
    ] {
        assert!(store.install_official(&p, &h).is_err());
    }
    assert!(store.install_official(&p2, &"0".repeat(64)).is_err());
    assert_eq!(store.install_official(&p2, &h2).unwrap(), 2);
    let active = store.official.join("active.json");
    let old_bytes = fs::read(&active).unwrap();
    let pending = store.official.join("active.json.pending");
    fs::create_dir(&pending).unwrap();
    let (p3, h3) = package(3, SCHEMA, LEGACY);
    assert!(store.install_official(&p3, &h3).is_err());
    assert_eq!(fs::read(&active).unwrap(), old_bytes);
    fs::remove_dir(&pending).unwrap();
    fs::write(&pending, "{ partial write").unwrap();
    assert_eq!(store.official().unwrap().0, 2);
    assert_eq!(store.install_official(&p3, &h3).unwrap(), 3);
    assert!(store.install_official(&p2, &h2).is_err());
}

#[test]
fn concurrent_updates_and_local_writes_preserve_isolation() {
    let root = Root::new();
    fs::write(root.0.join("tool"), b"tool bytes").unwrap();
    let store = Store::portable(&root.0).unwrap();
    let config = root.0.join("config");
    fs::create_dir(&config).unwrap();
    fs::write(config.join("settings.json"), b"configuration").unwrap();
    let (p2, h2) = package(2, SCHEMA, LEGACY);
    let (p3, h3) = package(3, SCHEMA, LEGACY);
    let (first, second) = thread::scope(|scope| {
        let a = scope.spawn(|| store.install_official(&p2, &h2));
        let b = scope.spawn(|| store.install_official(&p3, &h3));
        for i in 1..=6 {
            let store = &store;
            let root = &root;
            scope.spawn(move || store.record(sample(&root.0, 150_000, i, i + 4)).unwrap());
        }
        (a.join().unwrap(), b.join().unwrap())
    });
    assert!(first.is_ok() || first.unwrap_err().kind() == io::ErrorKind::InvalidData);
    assert_eq!(second.unwrap(), 3);
    assert_eq!(store.official().unwrap().0, 3);
    let key = sample(&root.0, 150_000, 10, 10);
    let history = store.history(key.profile()).unwrap().unwrap();
    assert_eq!(history.samples, 6);
    assert_eq!(history.largest_observed_bytes, 6);
    assert_eq!(history.slowest_observed_total, Duration::from_micros(10));
    assert_eq!(
        fs::read(config.join("settings.json")).unwrap(),
        b"configuration"
    );

    let local_file = store.local.join("production-v1.json");
    let previous = fs::read(&local_file).unwrap();
    let (p4, h4) = package(4, SCHEMA, LEGACY);
    store.install_official(&p4, &h4).unwrap();
    assert_eq!(fs::read(&local_file).unwrap(), previous);
    assert!(!String::from_utf8(previous)
        .unwrap()
        .contains("private source"));
}

#[test]
fn incomplete_mismatched_and_invalid_samples() {
    let root = Root::new();
    let executable = root.0.join("tool");
    fs::write(&executable, b"original").unwrap();
    let store = Store::portable(&root.0).unwrap();
    let cap = 150_000;
    let key = sample(&root.0, cap, 2, 10);
    assert!(store.history(key.profile()).unwrap().is_none());
    for i in 1..=2 {
        store.record(sample(&root.0, cap, i, 10)).unwrap();
    }
    assert!(store.history(key.profile()).unwrap().is_none());
    store.record(sample(&root.0, cap, 3, 10)).unwrap();
    assert!(store.history(key.profile()).unwrap().is_some());
    fs::write(executable, b"replacement").unwrap();
    assert!(store
        .history(sample(&root.0, cap, 3, 10).profile())
        .unwrap()
        .is_none());
    assert!(store.record(sample(&root.0, cap, cap + 1, 10)).is_err());

    let file = store.local.join("production-v1.json");
    fs::write(&file, b"{\"schema\":100,\"records\":[]}").unwrap();
    assert!(store.history(key.profile()).is_err());
    assert!(store.record(key).is_err());
    assert_eq!(fs::read(&file).unwrap(), b"{\"schema\":100,\"records\":[]}");
}

#[test]
fn s03_version_one_data_remains_readable() {
    let root = Root::new();
    let store = Store::portable(&root.0).unwrap();
    let mut data: Vec<serde_json::Value> = serde_json::from_str(LEGACY).unwrap();
    let mut historical = data.remove(0);
    historical["origin"] = "local".into();
    historical["source_report_sha256"] = "a".repeat(64).into();
    fs::create_dir_all(&store.local).unwrap();
    let path = store.local.join("s03-benchmark.json");
    let bytes = serde_json::to_vec(&vec![historical]).unwrap();
    fs::write(&path, &bytes).unwrap();
    let (version, db) = store.official().unwrap();
    assert_eq!(version, 1);
    assert_eq!(db.with_local(&path).unwrap().info()["local_samples"], 132);
    assert!(store.local().unwrap().records.is_empty());
    let (p2, h2) = package(2, SCHEMA, LEGACY);
    store.install_official(&p2, &h2).unwrap();
    assert_eq!(fs::read(path).unwrap(), bytes);
}
