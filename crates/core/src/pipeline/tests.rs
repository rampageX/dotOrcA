use super::*;

fn test_root() -> Stage {
    Stage::create(&std::env::temp_dir()).unwrap()
}

#[test]
fn publish_is_no_clobber_and_cancellation_cleans_stage() {
    let root = test_root();
    let stage = Stage::create(&root.0).unwrap();
    let staging_dir = stage.0.clone();
    let input = stage.0.join("source");
    let output = root.0.join("destination.mp4");
    fs::write(&input, b"verified").unwrap();
    let cancel = AtomicBool::new(true);
    assert!(matches!(
        publish(&input, &output, &cancel),
        Err(Error::Cancelled)
    ));
    assert!(!output.exists());
    cancel.store(false, Ordering::Release);
    fs::write(&output, b"existing").unwrap();
    assert!(matches!(
        publish(&input, &output, &cancel),
        Err(Error::DestinationExists)
    ));
    assert_eq!(fs::read(&output).unwrap(), b"existing");
    fs::remove_file(&output).unwrap();
    publish(&input, &output, &cancel).unwrap();
    drop(stage);
    assert_eq!(fs::read(&output).unwrap(), b"verified");
    assert!(!staging_dir.exists());
    assert!(matches!(
        io_error(io::ErrorKind::StorageFull.into(), &cancel),
        Error::DiskFull
    ));
}

fn tool(name: &str) -> PathBuf {
    let path = std::env::var_os(name).unwrap_or_else(|| panic!("{name} required"));
    fs::canonicalize(path).unwrap()
}

fn run_fixture(tool: &Path, args: &[&str], path: &Path) {
    let request = Request::new(tool)
        .args(args.iter().copied())
        .args([path.as_os_str()]);
    let out = process::run(&request, &AtomicBool::new(false)).unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr.bytes)
    );
}

#[test]
#[ignore = "S06 CI supplies external FFmpeg and FFprobe"]
fn production_special_path_vfr_audio_and_hdr() {
    decision::allow_fixture_fallback();
    let root = test_root();
    let ffmpeg = tool("DOTORCA_TEST_FFMPEG");
    let ffprobe = tool("DOTORCA_TEST_FFPROBE");
    let cancel = AtomicBool::new(false);
    let mut detector = Detector::default();
    let source = root.0.join("小K 的鹅腿坟场 & (黑洞) ^ % !.avi");
    let destination = root.0.join("-输出 🎬 & ( ) ^ % !.mp4");
    run_fixture(
        &ffmpeg,
        &[
            "-hide_banner",
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
        ],
        &source,
    );
    let mut request = EncodeRequest {
        source: &source,
        destination: &destination,
        ffmpeg: &ffmpeg,
        ffprobe: &ffprobe,
        limit_bytes: 180_000,
        codec: Codec::H264,
        max_long_edge: 256,
        timeout: Duration::from_secs(60),
    };
    let logs = |e: Event<'_>| match e {
        Event::Decision(_) => {}
        _ => eprintln!("{e:?}"),
    };
    let first = encode(&request, &mut detector, &cancel, logs).unwrap();
    assert!(first.bytes <= request.limit_bytes && first.bytes > 0);
    let cancel_out = root.0.join("cancelled.mp4");
    request.destination = &cancel_out;
    assert!(matches!(
        encode(
            &request,
            &mut detector,
            &cancel,
            |event| {
                if matches!(event, Event::Encoding { .. }) {
                    cancel.store(true, Ordering::Release);
                }
            },
        ),
        Err(Error::Cancelled)
    ));
    assert!(!cancel_out.exists());
    cancel.store(false, Ordering::Release);
    request.destination = &destination;
    let existing = fs::read(&destination).unwrap();
    assert!(matches!(
        encode(&request, &mut detector, &cancel, |_| {}),
        Err(Error::DestinationExists)
    ));
    request.destination = &source;
    assert!(matches!(
        encode(&request, &mut detector, &cancel, |_| {}),
        Err(Error::InvalidDestination)
    ));
    assert_eq!(fs::read(&destination).unwrap(), existing);
    let hdr = root.0.join("HDR 🎬 & ( ) ^ % !.mp4");
    let hdr_out = root.0.join("HLG output.mp4");
    run_fixture(
        &ffmpeg,
        &[
            "-hide_banner",
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=320x180:rate=25:duration=2",
            "-itsoffset",
            "0.12",
            "-f",
            "lavfi",
            "-i",
            "sine=sample_rate=48000:duration=1.75",
            "-vf",
            "select='not(eq(mod(n,5),2))',setsar=4/3,format=yuv420p10le",
            "-fps_mode",
            "passthrough",
            "-c:v",
            "libx265",
            "-x265-params",
            "pools=1:frame-threads=1",
            "-color_range",
            "tv",
            "-colorspace",
            "bt2020nc",
            "-color_primaries",
            "bt2020",
            "-color_trc",
            "arib-std-b67",
            "-c:a",
            "aac",
            "-f",
            "mp4",
        ],
        &hdr,
    );
    request.source = &hdr;
    request.destination = &hdr_out;
    request.codec = Codec::Hevc;
    let first = encode(&request, &mut detector, &cancel, logs).unwrap();
    assert!(first.bytes <= request.limit_bytes);
    request.destination = &hdr;
    assert!(matches!(
        encode(&request, &mut detector, &cancel, |_| {}),
        Err(Error::DestinationExists)
    ));
    let pq = root.0.join("HDR10 source.mp4");
    let pq_out = root.0.join("HDR10 output.mp4");
    run_fixture(
        &ffmpeg,
        &[
            "-hide_banner",
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=320x180:rate=25:duration=1",
            "-pix_fmt",
            "yuv420p10le",
            "-c:v",
            "libx265",
            "-x265-params",
            "pools=1:frame-threads=1:log-level=none:colorprim=bt2020:transfer=smpte2084:colormatrix=bt2020nc:master-display=G(13250,34500)B(7500,3000)R(34000,16000)WP(15635,16450)L(10000000,50):max-cll=1000,400",
            "-color_trc",
            "smpte2084",
            "-color_primaries",
            "bt2020",
            "-colorspace",
            "bt2020nc",
        ],
        &pq,
    );
    request.source = &pq;
    request.destination = &pq_out;
    let first = encode(&request, &mut detector, &cancel, logs).unwrap();
    assert!(first.bytes <= request.limit_bytes);

    let pq_result = media::probe(&ffprobe, &pq_out, &cancel).unwrap();
    assert_eq!(pq_result.streams[0].hdr, Hdr::Pq);
    assert_eq!(pq_result.streams[0].bit_depth, Some(10));

    let av1_out = root.0.join("AV1 HLG output.mp4");
    request.source = &hdr;
    request.destination = &av1_out;
    request.codec = Codec::Av1;
    let first = encode(&request, &mut detector, &cancel, logs).unwrap();
    assert!(first.bytes <= request.limit_bytes);

    let interlaced = root.0.join("50 fields with subtitles.mkv");
    let subtitles = root.0.join("captions.srt");
    fs::write(&subtitles, "1\n00:00:00,200 --> 00:00:01,800\nTest text\n").unwrap();
    let fixture = Request::new(&ffmpeg)
        .args([
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=320x180:rate=50:duration=2",
            "-i",
        ])
        .args([subtitles.as_os_str()])
        .args([
            "-vf",
            "tinterlace=interleave_top",
            "-c:v",
            "mpeg2video",
            "-flags:v",
            "+ilme+ildct",
            "-c:s",
            "srt",
        ])
        .args([interlaced.as_os_str()]);
    let fixture = process::run(&fixture, &cancel).unwrap();
    assert!(fixture.status.success(), "{:?}", fixture.stderr);
    let progressive = root.0.join("50p with subtitles.mp4");
    request.source = &interlaced;
    request.destination = &progressive;
    request.codec = Codec::H264;
    request.limit_bytes = 1_048_576;
    encode(&request, &mut detector, &cancel, |event| {
        assert!(!matches!(event, Event::Preflight { .. }));
    })
    .unwrap();
    let converted = media::probe(&ffprobe, &progressive, &cancel).unwrap();
    assert_eq!(converted.streams[0].raw["field_order"], "progressive");
    assert_eq!(converted.streams[0].avg_rate.unwrap().numerator, 50);
    assert_eq!(converted.streams[0].avg_rate.unwrap().denominator, 1);
    assert_eq!(converted.streams[1].codec.as_deref(), Some("mov_text"));

    #[cfg(target_os = "linux")]
    {
        let full = Request::new(&ffmpeg).args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-xerror",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=320x180:rate=25:duration=1",
            "-c:v",
            "libx264",
            "-f",
            "mp4",
            "/dev/full",
        ]);
        assert!(matches!(
            run_encoder(&full, "libx264", &cancel),
            Err(Error::DiskFull)
        ));
    }
    #[cfg(windows)]
    if let Some(full_root) = std::env::var_os("DOTORCA_TEST_FULL_ROOT") {
        use std::io::Write;

        let full_root = PathBuf::from(full_root);
        let output = full_root.join("disk-full.mp4");
        let filler = full_root.join("fixture-filler.bin");
        request.source = &source;
        request.destination = &output;
        request.codec = Codec::H264;
        let before = fs::read(&source).unwrap();
        let mut attempts = 0;
        let result = encode(&request, &mut detector, &cancel, |event| {
            if matches!(event, Event::Encoding { .. }) {
                attempts += 1;
                let mut file = File::options()
                    .write(true)
                    .create_new(true)
                    .open(&filler)
                    .unwrap();
                let block = vec![0; 1024 * 1024];
                for size in [block.len(), 4096] {
                    loop {
                        match file.write_all(&block[..size]) {
                            Ok(()) => {}
                            Err(error) if error.kind() == io::ErrorKind::StorageFull => break,
                            Err(error) => panic!("unexpected fixture write failure: {error}"),
                        }
                        assert!(
                            file.metadata().unwrap().len() <= 32 * 1024 * 1024,
                            "fixture must be run on a dedicated 32 MiB test volume"
                        );
                    }
                }
            }
        });
        assert!(matches!(result, Err(Error::DiskFull)), "{result:?}");
        assert_eq!(attempts, 1, "disk exhaustion must not trigger fallback");
        assert_eq!(fs::read(&source).unwrap(), before);
        assert!(!output.exists());
        fs::remove_file(&filler).unwrap();
        assert_eq!(fs::read_dir(&full_root).unwrap().count(), 0);
    }
    assert_eq!(fs::read_dir(&root.0).unwrap().count(), 10);
}
