//! S02 measurement only. No prediction or encoding policy.
use crate::{hardware, media, process::{self, Request, Stop}};
use serde_json::{json, Value};
use std::{fs, io::{self, Write}, path::{Path, PathBuf}, sync::atomic::{AtomicBool, Ordering}, time::{Duration, Instant, SystemTime, UNIX_EPOCH}};

#[cfg(windows)]
#[allow(unsafe_code)]
mod windows;
fn system_snapshot()->Value {
    #[cfg(windows)] { windows::snapshot() }
    #[cfg(not(windows))] { json!({"kernel_release":fs::read_to_string("/proc/sys/kernel/osrelease").ok().map(|s|s.trim().to_owned()),"os_release":fs::read_to_string("/etc/os-release").ok(),"memory":fs::read_to_string("/proc/meminfo").ok().map(|s|s.lines().take(3).collect::<Vec<_>>().join("\n"))}) }
}

pub const SCHEMA: u32 = 1;
pub const MATRIX: &str = "synthetic-sdr-v1";
const CANCEL: &str = "cancel.flag";

#[derive(Clone, Copy, Debug)]
struct Fixture { width: u32, height: u32, bits: u8, noisy: bool, frames: u32 }
impl Fixture {
    fn id(self) -> String { format!("{}x{}-{}bit-{}", self.width, self.height, self.bits, if self.noisy {"noise"} else {"motion"}) }
    fn pix(self) -> &'static str { if self.bits == 10 {"yuv420p10le"} else {"yuv420p"} }
    fn source(self) -> String {
        let noise = if self.noisy {",noise=alls=16:allf=t+u:all_seed=12345"} else {""};
        format!("testsrc2=size={}x{}:rate=30,format=yuv420p{noise},format={}", self.width,self.height,self.pix())
    }
    fn bitrate(self) -> u64 { match self.height { 0..=360 => 400_000, 361..=720 => 2_500_000, 721..=1080 => 5_000_000, _ => 18_000_000 } }
}
#[derive(Clone, Copy, Debug)]
struct Encoder { name: &'static str, codec: &'static str, option: &'static str, presets: [&'static str; 2], hardware: bool }
const ENCODERS: [Encoder; 12] = [
    Encoder{name:"libx264",codec:"h264",option:"-preset",presets:["fast","medium"],hardware:false},
    Encoder{name:"libx265",codec:"hevc",option:"-preset",presets:["fast","medium"],hardware:false},
    Encoder{name:"libsvtav1",codec:"av1",option:"-preset",presets:["8","6"],hardware:false},
    Encoder{name:"h264_nvenc",codec:"h264",option:"-preset",presets:["p4","p6"],hardware:true},
    Encoder{name:"hevc_nvenc",codec:"hevc",option:"-preset",presets:["p4","p6"],hardware:true},
    Encoder{name:"av1_nvenc",codec:"av1",option:"-preset",presets:["p4","p6"],hardware:true},
    Encoder{name:"h264_qsv",codec:"h264",option:"-preset",presets:["fast","slow"],hardware:true},
    Encoder{name:"hevc_qsv",codec:"hevc",option:"-preset",presets:["fast","slow"],hardware:true},
    Encoder{name:"av1_qsv",codec:"av1",option:"-preset",presets:["fast","slow"],hardware:true},
    Encoder{name:"h264_amf",codec:"h264",option:"-quality",presets:["speed","quality"],hardware:true},
    Encoder{name:"hevc_amf",codec:"hevc",option:"-quality",presets:["speed","quality"],hardware:true},
    Encoder{name:"av1_amf",codec:"av1",option:"-quality",presets:["speed","quality"],hardware:true},
];

pub struct Options { pub ffmpeg: PathBuf, pub ffprobe: PathBuf, pub output: PathBuf, pub smoke: bool, pub repeats: u8 }
fn invalid(msg: impl Into<String>) -> io::Error { io::Error::new(io::ErrorKind::InvalidData,msg.into()) }
fn request(exe: &Path) -> Request {
    let mut r = Request::new(exe);
    r.timeout = Duration::from_secs(1800);
    r.stderr_limit = 1024*1024;
    r
}
fn ff(exe: &Path) -> Request { request(exe).args(["-hide_banner","-nostdin","-n","-nostats"]) }
fn checked(r: &Request, cancel: &AtomicBool) -> io::Result<process::Outcome> {
    let o = process::run(r,cancel)?;
    if o.stop != Stop::Exited || !o.status.success() {
        return Err(io::Error::other(format!("{:?}: {}; stderr discarded={}; {}",o.stop,o.status,o.stderr.discarded,String::from_utf8_lossy(&o.stderr.bytes))));
    }
    if o.stdout.discarded != 0 { return Err(invalid("stdout capture truncated")); }
    Ok(o)
}
fn stdout(r: &Request, cancel: &AtomicBool) -> io::Result<String> {
    String::from_utf8(checked(r,cancel)?.stdout.bytes).map_err(|e|invalid(e.to_string()))
}
fn hash(r: &Request, cancel: &AtomicBool) -> io::Result<String> {
    let s=stdout(r,cancel)?;
    let h=s.trim().strip_prefix("SHA256=").ok_or_else(||invalid("missing SHA256"))?;
    if h.len()!=64 || !h.bytes().all(|c|c.is_ascii_hexdigit()) {return Err(invalid("invalid SHA256"));}
    Ok(h.to_ascii_lowercase())
}
fn file_hash(exe: &Path, p: &Path, cancel: &AtomicBool) -> io::Result<String> {
    hash(&ff(exe).args(["-v","error","-f","data","-i"]).args([p.as_os_str()]).args(["-map","0:0","-c","copy","-f","hash","-hash","sha256","-"]),cancel)
}
fn decoded_hash(exe: &Path, p: &Path, f: Fixture, cancel: &AtomicBool) -> io::Result<String> {
    hash(&ff(exe).args(["-v","error","-i"]).args([p.as_os_str()]).args(["-map","0:v:0","-c:v","rawvideo","-threads:v","1","-pix_fmt",f.pix(),"-f","hash","-hash","sha256","-"]),cancel)
}
fn frames(s: &str) -> io::Result<u64> {
    if !s.lines().any(|l|l.trim()=="progress=end") {return Err(invalid("missing final progress"));}
    s.lines().filter_map(|l|l.strip_prefix("frame=")).next_back().and_then(|x|x.trim().parse().ok()).ok_or_else(||invalid("missing frame count"))
}
fn ensure_frames(o: &process::Outcome, f: Fixture) -> io::Result<()> {
    if frames(&String::from_utf8_lossy(&o.stdout.bytes))? != u64::from(f.frames) {return Err(invalid("frame count mismatch"));}
    let text=String::from_utf8_lossy(&o.stdout.bytes);
    for key in ["dup_frames=","drop_frames="] {if text.lines().filter_map(|l|l.strip_prefix(key)).any(|v|v.trim().parse::<u64>().ok()!=Some(0)){return Err(invalid("frame duplication/drop"));}}
    Ok(())
}
fn listed(list: &str, name: &str) -> bool {
    list.lines().any(|l|{let mut w=l.split_whitespace();w.next().is_some_and(|x|x.len()==6&&x.starts_with('V'))&&w.next()==Some(name)})
}
fn fixtures(smoke: bool) -> Vec<Fixture> {
    if smoke {return vec![Fixture{width:320,height:180,bits:8,noisy:false,frames:12},Fixture{width:320,height:180,bits:10,noisy:true,frames:12}];}
    let mut v=Vec::new();
    for (width,height) in [(1280,720),(1920,1080),(3840,2160)] {for bits in [8,10] {for noisy in [false,true] {v.push(Fixture{width,height,bits,noisy,frames:90});}}}
    v
}
fn media_record(m: &media::MediaInfo, f: Fixture, codec: &str) -> io::Result<Value> {
    let v=m.streams.iter().find(|v|v.kind.as_deref()==Some("video")&&!v.attached_picture).ok_or_else(||invalid("missing video stream"))?;
    if v.width!=Some(f.width.into())||v.height!=Some(f.height.into())||v.bit_depth!=Some(f.bits.into())||v.codec.as_deref()!=Some(codec)||v.avg_rate!=Some(media::Rational{numerator:30,denominator:1}) {
        return Err(invalid(format!("unexpected output stream: {:?}",v)));
    }
    if m.streams.len()!=1 {return Err(invalid("expected one video stream"));}
    Ok(json!({"size_bytes":m.size_bytes,"duration_us":m.duration_us,"format":m.format,"stream":v.raw}))
}
fn count_frames(probe: &Path, p: &Path, f: Fixture, cancel: &AtomicBool) -> io::Result<()> {
    let s=stdout(&request(probe).args(["-v","error","-select_streams","v:0","-count_frames","-show_entries","stream=nb_read_frames","-of","default=nw=1:nk=1","-i"]).args([p.as_os_str()]),cancel)?;
    if s.trim().parse::<u64>().ok()!=Some(f.frames.into()) {return Err(invalid(format!("decoded frame count mismatch: {s}")));}
    Ok(())
}
fn create_fixture(o: &Options, f: Fixture, path: &Path, cancel: &AtomicBool) -> io::Result<Value> {
    let r=ff(&o.ffmpeg).args(["-v","error","-progress","pipe:1","-filter_threads","1","-f","lavfi","-i",&f.source(),"-frames:v",&f.frames.to_string(),"-an","-c:v","ffv1","-threads:v","1","-pix_fmt",f.pix(),"-fflags","+bitexact","-flags:v","+bitexact","-color_range","tv","-colorspace","bt709","-color_primaries","bt709","-color_trc","bt709"]).args([path.as_os_str()]);
    ensure_frames(&checked(&r,cancel)?,f)?;
    let m=media::probe(&o.ffprobe,path,cancel)?;
    let record=media_record(&m,f,"ffv1")?;
    count_frames(&o.ffprobe,path,f,cancel)?;
    Ok(json!({"id":f.id(),"generator":f.source(),"frames":f.frames,"fps":{"numerator":30,"denominator":1},"width":f.width,"height":f.height,"bit_depth":f.bits,"target_bitrate_bps":f.bitrate(),"file_sha256":file_hash(&o.ffmpeg,path,cancel)?,"decoded_sha256":decoded_hash(&o.ffmpeg,path,f,cancel)?,"media":record}))
}
fn metric_value(stderr: &str, filter: &str, key: &str) -> io::Result<Value> {
    let line=stderr.lines().rev().find(|l|l.contains(filter)&&l.contains(key)).ok_or_else(||invalid(format!("missing {filter} summary")))?;
    let token=line.split_whitespace().find_map(|w|w.strip_prefix(key)).ok_or_else(||invalid("missing metric value"))?;
    if token=="inf" {return Ok(json!("inf"));}
    let n=token.parse::<f64>().map_err(|_|invalid("invalid metric number"))?;
    if !n.is_finite() {return Err(invalid("non-finite metric"));}
    Ok(json!(n))
}
fn metric(o: &Options, f: Fixture, source: &Path, encoded: &Path, filter: &str, key: &str, cancel: &AtomicBool) -> io::Result<Value> {
    // No user paths in the filter expression. Explicit normalization of timebase and pixel format.
    let graph=format!("[0:v]settb=1/30,setpts=PTS-STARTPTS,format={}[a];[1:v]settb=1/30,setpts=PTS-STARTPTS,format={}[b];[a][b]{filter}=shortest=1:repeatlast=0",f.pix(),f.pix());
    let r=ff(&o.ffmpeg).args(["-v","info","-progress","pipe:1","-i"]).args([encoded.as_os_str()]).args(["-i"]).args([source.as_os_str()]).args(["-filter_complex_threads","1","-filter_complex",&graph,"-an","-f","null","-"]);
    let x=checked(&r,cancel)?;
    ensure_frames(&x,f)?;
    if x.stderr.discarded!=0 {return Err(invalid("metric log truncated"));}
    metric_value(&String::from_utf8_lossy(&x.stderr.bytes),if filter=="ssim" {"SSIM"} else {"PSNR"},key)
}
fn encode(o: &Options,f: Fixture,e: Encoder,preset: &str,source: &Path,out: &Path,cancel: &AtomicBool,record: &mut Value) -> io::Result<()> {
    let pix=if e.hardware {if f.bits==10 {"p010le"} else {"nv12"}} else {f.pix()};
    let r=ff(&o.ffmpeg).args(["-v","info","-progress","pipe:1","-i"]).args([source.as_os_str()]).args(["-map","0:v:0","-an","-fps_mode","passthrough","-vf",&format!("scale=iw:ih,format={pix}"),"-c:v",e.name,e.option,preset,"-b:v",&f.bitrate().to_string(),"-pix_fmt",&format!("+{pix}"),"-color_range","tv","-colorspace","bt709","-color_primaries","bt709","-color_trc","bt709"]).args([out.as_os_str()]);
    record["argv"]=json!(r.args.iter().map(|a|a.to_string_lossy()).collect::<Vec<_>>());
    let start=Instant::now();
    let x=process::run(&r,cancel);
    let elapsed=start.elapsed().as_secs_f64();
    record["encode_wall_seconds"]=json!(elapsed);
    let x=x?;
    record["encoder_log"]=json!(String::from_utf8_lossy(&x.stderr.bytes));
    record["encoder_log_discarded_bytes"]=json!(x.stderr.discarded);
    record["exit_code"]=json!(x.status.code());
    record["stop"]=json!(format!("{:?}",x.stop));
    if x.stop!=Stop::Exited||!x.status.success()||x.stdout.discarded!=0{return Err(io::Error::other(format!("encode {:?}: {}",x.stop,x.status)));}
    ensure_frames(&x,f)?;
    record["encoded_frames"]=json!(f.frames);
    record["fps"]=json!(f64::from(f.frames)/elapsed);
    let m=media::probe(&o.ffprobe,out,cancel)?;
    record["output_media"]=media_record(&m,f,e.codec)?;
    count_frames(&o.ffprobe,out,f,cancel)?;
    let size=m.size_bytes.ok_or_else(||invalid("missing output size"))?;
    record["output_bytes"]=json!(size);
    // CFR duration is an exact known property of this test, not rounded container metadata.
    record["actual_container_bitrate_bps"]=json!(size as f64*8.0*30.0/f64::from(f.frames));
    record["output_sha256"]=json!(file_hash(&o.ffmpeg,out,cancel)?);
    let q=Instant::now();
    record["ssim"]=metric(o,f,source,out,"ssim","All:",cancel)?;
    record["psnr_db"]=metric(o,f,source,out,"psnr","average:",cancel)?;
    record["quality_wall_seconds"]=json!(q.elapsed().as_secs_f64());
    Ok(())
}
fn save(path: &Path, value: &Value) -> io::Result<()> {
    let tmp=path.with_extension("tmp");
    let mut file=fs::OpenOptions::new().write(true).create_new(true).open(&tmp)?;
    serde_json::to_writer_pretty(&mut file,value).map_err(|e|io::Error::other(e.to_string()))?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    drop(file);
    fs::rename(&tmp,path)
}
struct Work(PathBuf);
impl Drop for Work {fn drop(&mut self){if let Err(e)=fs::remove_dir_all(&self.0){eprintln!("Work cleanup failed: {e}");}}}
fn unix_ms() -> u128 {SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis()}
fn run_inner(o: &Options, root: &Path, cancel: &AtomicBool) -> io::Result<()> {
    let work=Work(root.join("work"));
    fs::create_dir(&work.0)?;
    let cases=root.join("cases");fs::create_dir(&cases)?;
    let report=hardware::Detector::default().detect(&o.ffmpeg,cancel)?;
    let list=stdout(&request(&o.ffmpeg).args(["-hide_banner","-encoders"]),cancel)?;
    let probe_version=stdout(&request(&o.ffprobe).args(["-version"]),cancel)?;
    let env=json!({
        "os":std::env::consts::OS,"arch":std::env::consts::ARCH,"system_snapshot":system_snapshot(),
        "cpu":report.inventory.cpu,"logical_cpus":report.inventory.logical_cpus,
        "adapters":report.inventory.adapters.iter().map(|a|json!({"name":a.name,"driver_version":a.driver_version})).collect::<Vec<_>>(),
        "adapter_scan_complete":report.inventory.adapter_scan_complete,"adapter_scan_error":report.inventory.adapter_scan_error,
        "hardware_evidence":report.capabilities.iter().map(|c|json!({"encoder":c.encoder,"bit_depth":c.bit_depth,"evidence":format!("{:?}",c.evidence),"diagnostic":c.diagnostic})).collect::<Vec<_>>(),
        "hardware_device_selection":"FFmpeg backend default; inventory is not per-adapter attribution",
        "ffmpeg_version":report.tool_version,"ffprobe_version":probe_version,
        "ffmpeg_sha256":file_hash(&o.ffmpeg,&o.ffmpeg,cancel)?,"ffprobe_sha256":file_hash(&o.ffmpeg,&o.ffprobe,cancel)?,
        "benchmark_version":env!("CARGO_PKG_VERSION"),"benchmark_source":option_env!("DOTORCA_SOURCE_SHA"),
        "started_unix_ms":unix_ms() as u64,"repeats":o.repeats,"profile":if o.smoke {"smoke"} else {"standard"},
        "cpu_threads":"encoder defaults","decode":"software","timing":"spawn through drained process exit, includes decode/filter/mux; excludes probes and quality"
    });
    save(&root.join("environment.json"),&env)?;
    let mut records=Vec::new();let mut sources=Vec::new();
    let mut failures=0u64;let mut successes=0u64;
    for f in fixtures(o.smoke) {
        if cancel.load(Ordering::Acquire){break;}
        let source=work.0.join("小K 的鹅腿坟场 & (黑洞) ^ % !.mkv");
        let fixture=create_fixture(o,f,&source,cancel)?;
        sources.push(fixture.clone());
        save(&root.join(format!("fixture-{}.json",f.id())),&fixture)?;
        for e in ENCODERS {
            if o.smoke && !["libx264","libx265","libsvtav1","h264_nvenc"].contains(&e.name) {continue;}
            for preset in e.presets {for rep in 1..=o.repeats {
                if cancel.load(Ordering::Acquire){break;}
                let id=format!("{}-{}-{}-{}",f.id(),e.name,preset,rep);
                println!("{id}");
                let mut v=json!({"schema_version":SCHEMA,"matrix_version":MATRIX,"case_id":id,"fixture_id":f.id(),"fixture_decoded_sha256":fixture["decoded_sha256"],"encoder":e.name,"codec":e.codec,"bit_depth":f.bits,"preset":preset,"repeat":rep,"target_bitrate_bps":f.bitrate(),"status":"pending"});
                let skip=if !listed(&list,e.name) {Some("not_listed".to_string())}
                    else if e.hardware {report.capabilities.iter().find(|c|c.encoder==e.name&&c.bit_depth==f.bits).and_then(|c|if c.evidence!=hardware::Evidence::Verified{Some(format!("probe_{:?}",c.evidence))}else{None})}
                    else {None};
                if let Some(reason)=skip {v["status"]=json!("skipped");v["reason"]=json!(reason);}
                else {
                    let out=work.0.join("encoded.mkv");
                    let result=encode(o,f,e,preset,&source,&out,cancel,&mut v);
                    match result {
                        Ok(())=>{v["status"]=json!("ok");successes+=1;}
                        Err(err)=>{v["status"]=json!(if cancel.load(Ordering::Acquire){"cancelled"}else{"failed"});v["error"]=json!(err.to_string());failures+=1;}
                    }
                    if out.exists(){fs::remove_file(&out)?;}
                }
                println!("  {}",v["status"].as_str().unwrap_or("?"));
                save(&cases.join(format!("{id}.json")),&v)?;
                records.push(v);
            }}
        }
        fs::remove_file(&source)?;
    }
    let cancelled=cancel.load(Ordering::Acquire);
    let skipped=records.iter().filter(|r|r["status"]=="skipped").count();
    save(&root.join("report.json"),&json!({"schema_version":SCHEMA,"matrix_version":MATRIX,"metric_version":"ffmpeg-ssim-psnr-v1","environment":env,"fixtures":sources,"records":records,"successes":successes,"failures":failures,"skipped":skipped,"cancelled":cancelled}))?;
    if cancelled || failures>0 || successes==0 {return Err(io::Error::other(format!("Incomplete benchmark: ok={successes}, failed={failures}, cancelled={cancelled}")));}
    Ok(())
}
pub fn run(mut o: Options) -> io::Result<PathBuf> {
    if !(1..=5).contains(&o.repeats){return Err(invalid("repeats must be 1..5"));}
    o.ffmpeg=fs::canonicalize(&o.ffmpeg)?;o.ffprobe=fs::canonicalize(&o.ffprobe)?;
    fs::create_dir_all(&o.output)?;
    let root=fs::canonicalize(&o.output)?.join(format!("run-{}-{}",unix_ms(),std::process::id()));
    fs::create_dir(&root)?;
    println!("Results: {}",root.display());
    println!("To cancel safely, create an empty file named {CANCEL} in that directory.");
    let cancel=AtomicBool::new(false);let finished=AtomicBool::new(false);
    let result=std::thread::scope(|s|{
        s.spawn(||{while !finished.load(Ordering::Acquire){if root.join(CANCEL).exists(){cancel.store(true,Ordering::Release);break;}std::thread::sleep(Duration::from_millis(100));}});
        let result=run_inner(&o,&root,&cancel);
        finished.store(true,Ordering::Release);
        result
    });
    result?;
    Ok(root)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn matrix_contract(){let f=fixtures(false);assert_eq!(f.len(),12);assert_eq!(ENCODERS.len(),12);assert_eq!(f.iter().filter(|f|f.bits==10).count(),6);assert!(f.iter().any(|f|f.height==2160&&f.noisy));}
    #[test] fn progress_and_metrics(){assert_eq!(frames("frame= 12\r\nprogress=end\r\n").unwrap(),12);assert!(frames("frame=12\nprogress=continue").is_err());assert_eq!(metric_value("[Parsed_ssim] SSIM Y:1.0 All:0.999 (30)", "SSIM","All:").unwrap(),json!(0.999));assert_eq!(metric_value("[Parsed_psnr] PSNR average:inf min:inf","PSNR","average:").unwrap(),json!("inf"));assert!(metric_value("PSNR average:nan","PSNR","average:").is_err());}
    #[test] fn exact_encoder_names(){assert!(listed(" V....D libx264 H.264","libx264"));assert!(!listed(" V....D libx264rgb H.264","libx264"));}
}
