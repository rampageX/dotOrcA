#![deny(unsafe_code)]
use orca_core::benchmark::{self, Options};
use std::{io, path::PathBuf};

fn usage() {
    println!("dotOrcA Benchmark S02\n\
Usage: orca-benchmark [--ffmpeg PATH] [--ffprobe PATH] [--output DIR] [--smoke] [--repeats 1..5]\n\
Default: tools/ffmpeg.exe and tools/ffprobe.exe next to EXE; output benchmark-results/ next to EXE.\n\
Default standard matrix: 720p/1080p/2160p, 8/10-bit, motion/noise, 180 frames at 30 fps.\n\
--smoke: two 320x180 fixtures, 12 frames; a reduced encoder set. Not training data.\n\
--repeats: repeat every case (default 1). Runs are serial. No files are uploaded.");
}
fn options() -> io::Result<Option<Options>> {
    let exe=std::env::current_exe()?;
    let base=exe.parent().ok_or_else(||io::Error::other("EXE has no parent"))?;
    let suffix=std::env::consts::EXE_SUFFIX;
    let mut o=Options{ffmpeg:base.join("tools").join(format!("ffmpeg{suffix}")),ffprobe:base.join("tools").join(format!("ffprobe{suffix}")),output:base.join("benchmark-results"),smoke:false,repeats:1};
    let mut args=std::env::args_os().skip(1);
    while let Some(a)=args.next() {
        let missing=||io::Error::new(io::ErrorKind::InvalidInput,format!("missing value for {}",a.to_string_lossy()));
        match a.to_str() {
            Some("--help"|"-h")=>{usage();return Ok(None);}
            Some("--smoke")=>o.smoke=true,
            Some("--ffmpeg")=>o.ffmpeg=PathBuf::from(args.next().ok_or_else(missing)?),
            Some("--ffprobe")=>o.ffprobe=PathBuf::from(args.next().ok_or_else(missing)?),
            Some("--output")=>o.output=PathBuf::from(args.next().ok_or_else(missing)?),
            Some("--repeats")=>o.repeats=args.next().and_then(|v|v.to_str()?.parse().ok()).ok_or_else(||io::Error::new(io::ErrorKind::InvalidInput,"invalid repeats"))?,
            _=>return Err(io::Error::new(io::ErrorKind::InvalidInput,format!("unknown argument: {}",a.to_string_lossy())))
        }
    }
    Ok(Some(o))
}
fn main() {
    let result=options().and_then(|o|match o {Some(o)=>benchmark::run(o).map(|p|println!("Report: {}",p.join("report.json").display())),None=>Ok(())});
    if let Err(e)=result {eprintln!("Benchmark: {e}");std::process::exit(1);}
}
