//! S01 developer acceptance CLI, not the production GUI.
use orca_core::{hardware,media,process::{self,Request,Stop}};
use std::{ffi::OsString,io::{self,Write},path::{Path,PathBuf},sync::{Arc,atomic::{AtomicBool,Ordering}},thread,time::{Duration,Instant}};

fn absolute(p:PathBuf)->io::Result<PathBuf>{std::path::absolute(p)}
fn arg_error(s:&str)->io::Error{io::Error::new(io::ErrorKind::InvalidInput,s)}
fn report_self_test()->io::Result<()> {
    let me=std::env::current_exe()?;
    let cancel=Arc::new(AtomicBool::new(false));
    let args=["小K 的鹅腿坟场 & (黑洞) ^ % !.avi","emoji-🫍-𠮷.avi","-leading.avi",r#"quote"inside"#,r#"trailing\"#,r#"C:\a b\\"#,r#"\\server\share\中文 & (x).avi"#,r#"\\?\C:\long path\中文.avi"#];
    let o=process::run(&Request::new(&me).args(["--child-argv"]).args(args),&cancel)?;
    let expected=serde_json::to_vec(&args).map_err(io::Error::other)?;
    if !o.status.success()||o.stdout.bytes!=expected {return Err(io::Error::other(format!("argv mismatch: {:?}",o)));}
    println!("PASS actual child argv (Unicode, special characters, quote, backslash, UNC syntax)");
    let mut flood=Request::new(&me).args(["--child-flood"]);
    flood.stdout_limit=1024;flood.stderr_limit=1024;
    let o=process::run(&flood,&cancel)?;
    if o.stop!=Stop::Exited||!o.status.success()||o.stdout.bytes.len()!=1024||o.stderr.bytes.len()!=1024||o.stdout.discarded!=1_048_576-1024||o.stderr.discarded!=1_048_576-1024{return Err(io::Error::other(format!("bounded dual pipe test: {:?}",o)));}
    println!("PASS simultaneous stdout/stderr, bounded capture");
    let o=process::run(&Request::new(&me).args(["--child-fail"]),&cancel)?;
    if o.status.code()!=Some(17)||o.stop!=Stop::Exited{return Err(io::Error::other("nonzero exit not preserved"));}
    println!("PASS nonzero exit status");
    let missing=me.with_file_name("dotorca-s01-no-such-tool-87d910.exe");
    if process::run(&Request::new(&missing),&cancel).is_ok(){return Err(io::Error::other("missing tool accepted"));}
    println!("PASS missing tool");
    // A grandchild inherits both output pipes and would keep the readers blocked.
    // A delayed marker provides external evidence beyond merely seeing parent exit.
    for mode in ["timeout","cancel","normal"] {
        let dir=Scratch::new()?;
        let ready=dir.0.join("ready");
        let marker=dir.0.join("survived");
        let mut req=Request::new(&me).args([OsString::from("--child-tree"),OsString::from(mode),ready.as_os_str().to_owned(),marker.as_os_str().to_owned()]);
        req.timeout=if mode=="timeout"{Duration::from_secs(3)}else{Duration::from_secs(8)};
        let start=Instant::now();
        let token=Arc::new(AtomicBool::new(false));
        let t=if mode=="cancel" {
            let t=token.clone();let p=ready.clone();
            Some(thread::spawn(move||{
                let start=Instant::now();
                while !p.exists()&&start.elapsed()<Duration::from_secs(5){thread::sleep(Duration::from_millis(10));}
                t.store(true,Ordering::Release);
            }))
        }else{None};
        let o=process::run(&req,&token)?;
        if let Some(t)=t {t.join().map_err(|_|io::Error::other("cancellation thread panicked"))?;}
        let expected=match mode{"timeout"=>Stop::TimedOut,"cancel"=>Stop::Cancelled,_=>Stop::Exited};
        if o.stop!=expected||!ready.exists()||start.elapsed()>=Duration::from_secs(7){return Err(io::Error::other(format!("tree {mode}: {:?}, ready={}",o,ready.exists())));}
        thread::sleep(Duration::from_secs(4));
        if marker.exists(){return Err(io::Error::other(format!("grandchild survived {mode}")));}
        println!("PASS process tree {mode} (grandchild started, no delayed marker, both pipes closed)");
    }
    println!("S01 process self-test PASS");
    Ok(())
}
struct Scratch(PathBuf);
impl Scratch{
    fn new()->io::Result<Self>{
        let base=std::env::temp_dir();
        for i in 0..100{
            let p=base.join(format!("dotorca-s01-{}-{i}",std::process::id()));
            match std::fs::create_dir(&p){Ok(())=>return Ok(Self(p)),Err(e) if e.kind()==io::ErrorKind::AlreadyExists=>continue,Err(e)=>return Err(e)}
        }
        Err(io::Error::other("cannot allocate scratch directory"))
    }
}
impl Drop for Scratch{fn drop(&mut self){let _=std::fs::remove_dir_all(&self.0);}}
fn child_mode(args:&[OsString])->io::Result<bool>{
    let Some(first)=args.first().and_then(|s|s.to_str()) else{return Ok(false)};
    match first{
        "--child-argv"=>{
            let strings=args[1..].iter().map(|s|s.to_str().ok_or_else(||arg_error("non-Unicode test argument"))).collect::<io::Result<Vec<_>>>()?;
            serde_json::to_writer(io::stdout().lock(),&strings).map_err(io::Error::other)?;
        }
        "--child-flood"=>{
            let a=thread::spawn(||io::stdout().lock().write_all(&vec![b'a';1_048_576]));
            io::stderr().lock().write_all(&vec![b'b';1_048_576])?;
            a.join().map_err(|_|io::Error::other("writer panic"))??;
        }
        "--child-fail"=>std::process::exit(17),
        "--child-tree"=>{
            if args.len()!=4{return Err(arg_error("child-tree arguments"));}
            let _grandchild=std::process::Command::new(std::env::current_exe()?).arg("--child-grandchild").args(&args[2..]).spawn()?;
            let start=Instant::now();
            while !Path::new(&args[2]).exists()&&start.elapsed()<Duration::from_secs(5){thread::sleep(Duration::from_millis(10));}
            if args[1]!="normal"{thread::sleep(Duration::from_secs(20));}
        }
        "--child-grandchild"=>{
            if args.len()!=3{return Err(arg_error("child-grandchild arguments"));}
            std::fs::write(&args[1],b"started")?;
            thread::sleep(Duration::from_secs(4));
            std::fs::write(&args[2],b"survived")?;
        }
        _=>return Ok(false)
    }
    Ok(true)
}

fn media_fixtures(ffmpeg:&Path,ffprobe:&Path)->io::Result<()>{
    let scratch=Scratch::new()?;
    let root=scratch.0.clone();
    let cancel=AtomicBool::new(false);
    let run_ffmpeg=|args:Vec<OsString>|->io::Result<()>{
        let o=process::run(&Request::new(ffmpeg).args(args),&cancel)?;
        if o.stop!=Stop::Exited||!o.status.success(){return Err(io::Error::other(format!("fixture: {:?}",o)));}
        Ok(())
    };
    let avi=root.join("小K 的鹅腿坟场 & (黑洞) ^ % !.avi");
    let mut args:Vec<OsString>=["-hide_banner","-loglevel","error","-nostdin","-f","lavfi","-i","testsrc2=s=128x96:r=30000/1001","-f","lavfi","-i","sine=frequency=1000:sample_rate=48000","-t","0.4","-c:v","mpeg4","-c:a","pcm_s16le","-y"].into_iter().map(Into::into).collect();
    args.push(avi.as_os_str().into());
    run_ffmpeg(args)?;
    let m=media::probe(ffprobe,&avi,&cancel)?;
    let v=m.streams.iter().find(|s|s.kind.as_deref()==Some("video")).ok_or_else(||io::Error::other("missing video"))?;
    if m.size_bytes!=Some(std::fs::metadata(&avi)?.len())||m.duration_us.is_none()||v.width!=Some(128)||v.height!=Some(96)||v.avg_rate!=Some(media::Rational{numerator:30000,denominator:1001})||v.bit_depth!=Some(8)||!m.streams.iter().any(|s|s.kind.as_deref()==Some("audio")&&s.sample_rate==Some(48000)){
        return Err(io::Error::other(format!("AVI fixture mismatch: {:?}",m)));
    }
    println!("PASS real AVI special filename, rational rate, actual bytes, audio");
    let long=root.join("长路径-𠮷-🫍-".repeat(8)).join("segment-".repeat(20)).join("another-".repeat(12));
    std::fs::create_dir_all(&long)?;
    let copy=long.join("-long & (path) ^ % !.avi");std::fs::copy(&avi,&copy)?;
    let m2=media::probe(ffprobe,&copy,&cancel)?;
    if m2.size_bytes!=m.size_bytes||m2.duration_us!=m.duration_us{return Err(io::Error::other("long path fixture mismatch"));}
    println!("PASS real long Unicode path");
    for (transfer,trc,expected) in [("smpte2084",16,media::Hdr::Pq),("arib-std-b67",18,media::Hdr::Hlg),("bt709",1,media::Hdr::Sdr)]{
        let path=root.join(format!("{transfer}.mp4"));
        // Mark actual input frames, codec VUI and MP4 colr. Do not assume
        // output AVCodecContext options alone survive filter/encoder negotiation.
        let source=format!("testsrc2=s=128x96:r=25,format=yuv420p10le,setparams=range=limited:color_primaries=bt2020:color_trc={transfer}:colorspace=bt2020nc");
        let params=format!("log-level=error:pools=1:frame-threads=1:colorprim=9:transfer={trc}:colormatrix=9:range=limited");
        let bsf=format!("hevc_metadata=colour_primaries=9:transfer_characteristics={trc}:matrix_coefficients=9:video_full_range_flag=0");
        let mut args:Vec<OsString>=["-hide_banner","-loglevel","error","-nostdin","-f","lavfi","-i",&source,"-frames:v","8","-c:v","libx265","-preset","ultrafast","-x265-params",&params,"-pix_fmt","yuv420p10le","-color_trc",transfer,"-color_primaries","bt2020","-colorspace","bt2020nc","-color_range","tv","-bsf:v",&bsf,"-tag:v","hvc1","-movflags","+write_colr","-y"].into_iter().map(Into::into).collect();
        args.push(path.as_os_str().into());run_ffmpeg(args)?;
        let m=media::probe(ffprobe,&path,&cancel)?;
        let v=&m.streams[0];
        if v.hdr!=expected||v.bit_depth!=Some(10)||v.color_transfer.as_deref()!=Some(transfer)||v.color_primaries.as_deref()!=Some("bt2020")||v.color_range.as_deref()!=Some("tv"){return Err(io::Error::other(format!("color fixture mismatch: {:?}",v)));}
        println!("PASS real {transfer} 10-bit color tags");
    }
    let invalid=root.join("invalid.avi");std::fs::write(&invalid,b"not a media file")?;
    if media::probe(ffprobe,&invalid,&cancel).is_ok(){return Err(io::Error::other("invalid media accepted"));}
    drop(scratch);
    if root.exists(){return Err(io::Error::other("owned scratch cleanup failed"));}
    println!("PASS invalid media and owned scratch cleanup");
    Ok(())
}

fn run()->io::Result<()> {
    let args:Vec<OsString>=std::env::args_os().skip(1).collect();
    if child_mode(&args)?{return Ok(());}
    if args.first().is_some_and(|s|s=="--self-test"){return report_self_test();}
    let root=std::env::current_exe()?.parent().ok_or_else(||io::Error::other("no executable directory"))?.to_owned();
    let suffix=std::env::consts::EXE_SUFFIX;
    let mut ffmpeg=root.join("tools").join(format!("ffmpeg{suffix}"));
    let mut ffprobe=root.join("tools").join(format!("ffprobe{suffix}"));
    let mut media_paths=Vec::new();let mut detect=true;let mut fixtures=false;let mut repeat=false;
    let mut i=0;
    while i<args.len(){
        match args[i].to_str(){
            Some("--ffmpeg"|"--ffprobe"|"--media")=>{
                let v=args.get(i+1).ok_or_else(||arg_error("missing argument value"))?;
                match args[i].to_str().unwrap(){"--ffmpeg"=>ffmpeg=absolute(v.into())?,"--ffprobe"=>ffprobe=absolute(v.into())?,_=>media_paths.push(PathBuf::from(v))}
                i+=2;
            }
            Some("--repeat-hardware")=>{repeat=true;i+=1;}
            Some("--ci-media")=>{fixtures=true;i+=1;}
            Some("--no-hardware")=>{detect=false;i+=1;}
            Some("--help")=>{println!("dotorca-s01 [--self-test] [--ffmpeg PATH] [--ffprobe PATH] [--no-hardware] [--repeat-hardware] [--media FILE ...]\nDefault tools directory is next to EXE. No source files are modified.");return Ok(());}
            _=>{media_paths.push(PathBuf::from(&args[i]));i+=1;}
        }
    }
    if fixtures {return media_fixtures(&ffmpeg,&ffprobe);}
    let cancel=AtomicBool::new(false);
    for path in media_paths {
        let info=media::probe(&ffprobe,&path,&cancel)?;
        println!("MEDIA: {}\nsize_bytes={:?} duration_us={:?} format={:?}",info.path.display(),info.size_bytes,info.duration_us,info.format);
        for s in &info.streams {
            println!("STREAM {:?} {:?} {:?}: {:?}x{:?} fps={:?} nominal={:?} depth={:?} hdr={:?} pix_fmt={:?} range={:?} space={:?} transfer={:?} primaries={:?} channels={:?} sample_rate={:?} attached_pic={}",
                s.index,s.kind,s.codec,s.width,s.height,s.avg_rate,s.nominal_rate,s.bit_depth,s.hdr,s.pixel_format,s.color_range,s.color_space,s.color_transfer,s.color_primaries,s.channels,s.sample_rate,s.attached_picture);
            if let Some(side)=s.raw.get("side_data_list"){println!("SIDE DATA: {side}");}
        }
    }
    if detect{
        let mut detector=hardware::Detector::default();
        let report=detector.detect(&ffmpeg,&cancel)?;
        println!("INVENTORY: {:#?}",report.inventory);
        if repeat {let again=detector.detect(&ffmpeg,&cancel)?;println!("SECOND DETECTION cache_hit={}",again.cache_hit); }
        println!("FFmpeg: {}",report.tool_version.lines().next().unwrap_or("unknown"));
        for c in report.capabilities{println!("{} {}-bit {:?}: {}",c.encoder,c.bit_depth,c.evidence,c.diagnostic.trim());}
    }
    Ok(())
}
fn main(){if let Err(e)=run(){eprintln!("ERROR: {e}");std::process::exit(1);}}
