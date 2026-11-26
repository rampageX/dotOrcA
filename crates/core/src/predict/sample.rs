//! Independent S03 short-source video-only pipeline validation.
use crate::{hardware,media,process::{self,Request,Stop}};
use serde_json::{json,Value};
use std::{fs,io,path::{Path,PathBuf},sync::atomic::AtomicBool,time::{Duration,Instant,SystemTime,UNIX_EPOCH}};
pub const VERSION:&str="sample-pipeline-v1";
fn bad(s:impl Into<String>)->io::Error{io::Error::other(s.into())}
#[derive(Clone,Copy,Debug)]
pub struct Sample{pub source_seconds:f64,pub wall_seconds:f64}
pub fn estimate(samples:&[Sample],seconds:f64)->Option<Value>{
    if samples.len()<2||!seconds.is_finite()||seconds<=0.0{return None}
    let(mut lo,mut hi,mut source,mut wall)=(f64::INFINITY,0.0_f64,0.0,0.0);
    for s in samples{
        if !s.source_seconds.is_finite()||s.source_seconds<=0.0||!s.wall_seconds.is_finite()||s.wall_seconds<=0.0{return None}
        let r=s.wall_seconds/s.source_seconds;lo=lo.min(r);hi=hi.max(r);source+=s.source_seconds;wall+=s.wall_seconds;
    }
    let point=wall/source*seconds;let lower=lo*seconds*0.5;let upper=hi*seconds*1.5;
    if ![point,lower,upper].iter().all(|v|v.is_finite()&&*v>0.0){return None}
    Some(json!({"model_version":VERSION,"kind":"inferred_same_source_video_pipeline","confidence":"low_uncalibrated","estimated_video_seconds":point,"heuristic_seconds_range":[lower,upper],"production_seconds":null,"isolated_encoder_fps":null}))
}
struct Temp(PathBuf);
impl Temp{
    fn new()->io::Result<Self>{
        let stamp=SystemTime::now().duration_since(UNIX_EPOCH).map_err(io::Error::other)?.as_nanos();
        let p=std::env::temp_dir().join(format!("dotOrcA-S03-{}-{stamp}",std::process::id()));
        fs::create_dir(&p)?;Ok(Self(p))
    }
}
impl Drop for Temp{fn drop(&mut self){for i in 0..6{let _=fs::remove_file(self.0.join(format!("{i}.mkv")));}let _=fs::remove_dir(&self.0);}}
#[derive(Clone,Copy)]
struct Route{encoder:&'static str,preset:&'static str,codec:&'static str,pix:&'static str}
fn measure(ffmpeg:&Path,ffprobe:&Path,input:&Path,index:u64,route:Route,bitrate:u64,offset:u64,duration:u64,out:&Path,cancel:&AtomicBool)->io::Result<(Sample,Value)>{
    let before=crate::benchmark::pretest_cpu_busy();
    let mut req=Request::new(ffmpeg).args(["-hide_banner","-nostdin","-n","-v","error","-progress","pipe:1","-nostats","-ss",&offset.to_string(),"-i"]).args([input.as_os_str()])
        .args(["-map",&format!("0:{index}"),"-an","-sn","-dn","-t",&duration.to_string(),"-fps_mode","passthrough","-vf",&format!("format={}",route.pix),"-c:v",route.encoder,"-preset",route.preset,"-b:v",&bitrate.to_string(),"-pix_fmt",&format!("+{}",route.pix),"-f","matroska"]).args([out.as_os_str()]);
    req.timeout=Duration::from_secs(60);
    let start=Instant::now();let o=process::run(&req,cancel)?;let wall=start.elapsed().as_secs_f64();
    if o.stop!=Stop::Exited||!o.status.success()||o.stdout.discarded!=0{return Err(bad(format!("{} {:?}: {}",route.encoder,o.stop,String::from_utf8_lossy(&o.stderr.bytes))))}
    let progress=String::from_utf8(o.stdout.bytes).map_err(io::Error::other)?;
    if !progress.lines().any(|s|s.trim()=="progress=end"){return Err(bad("missing final progress"))}
    let frames=progress.lines().filter_map(|s|s.strip_prefix("frame=")?.trim().parse::<u64>().ok()).last().filter(|n|*n>0).ok_or_else(||bad("missing frames"))?;
    let m=media::probe(ffprobe,out,cancel)?;
    let v=m.streams.iter().find(|s|s.kind.as_deref()==Some("video")).ok_or_else(||bad("missing output video"))?;
    if v.codec.as_deref()!=Some(route.codec)||v.bit_depth!=Some(8){return Err(bad("output codec/depth mismatch"))}
    let sec=m.duration_us.filter(|n|*n>0).ok_or_else(||bad("missing output duration"))? as f64/1e6;
    if (sec-duration as f64).abs()>0.5{return Err(bad("incomplete sample duration"))}
    let bytes=m.size_bytes.filter(|n|*n>0).ok_or_else(||bad("empty output"))?;fs::remove_file(out)?;
    Ok((Sample{source_seconds:sec,wall_seconds:wall},json!({"offset_seconds":offset,"requested_seconds":duration,"encoded_seconds":sec,"encoded_frames":frames,"wall_seconds":wall,"observed_pipeline_fps":frames as f64/wall,"output_bytes":bytes,"pretest_system_cpu_busy_pct":before,"pretest_gpu_busy_pct":null})))
}
/// Only the standalone validation command may accept untagged SDR by explicit
/// user confirmation. Never override known HDR, HDR side data or a known
/// unsupported transfer function. Keep the original S01 classification.
fn source_color(v:&media::Stream,assume_sdr:bool)->io::Result<&'static str>{
    let hdr_side=v.raw["side_data_list"].as_array().is_some_and(|a|a.iter().any(|x|{
        let t=x["side_data_type"].as_str().unwrap_or("").to_ascii_lowercase();
        t.contains("mastering display")||t.contains("content light")||t.contains("hdr")||t.contains("dolby vision")||t.contains("dovi")
    }));
    if hdr_side||matches!(v.hdr,media::Hdr::Pq|media::Hdr::Hlg|media::Hdr::DolbyVision){return Err(bad("HDR source is outside SDR validation scope"))}
    if v.bit_depth!=Some(8)||v.pixel_format.as_deref()!=Some("yuv420p"){return Err(bad(format!("requires 8-bit yuv420p; detected depth={:?}, pixel_format={:?}",v.bit_depth,v.pixel_format)))}
    match v.hdr{
        media::Hdr::Sdr=>Ok("metadata_sdr"),
        media::Hdr::Unknown if assume_sdr&&v.color_transfer.is_none()&&v.color_primaries.as_deref()!=Some("bt2020")&&v.color_space.as_deref()!=Some("bt2020nc")&&v.color_space.as_deref()!=Some("bt2020c")=>Ok("user_confirmed_untagged_sdr"),
        _=>Err(bad(format!("SDR not established: transfer={:?}, primaries={:?}, matrix={:?}; for known SDR with missing transfer only, use --assume-sdr",v.color_transfer,v.color_primaries,v.color_space)))
    }
}
/// Two 4s windows predict a disjoint 12s window. No source or dataset writes.
pub fn validate(ffmpeg:&Path,ffprobe:&Path,input:&Path,report:&hardware::Report,assume_sdr:bool,cancel:&AtomicBool)->io::Result<Value>{
    let m=media::probe(ffprobe,input,cancel)?;
    let mut videos=m.streams.iter().filter(|s|s.kind.as_deref()==Some("video")&&!s.attached_picture);
    let v=videos.next().ok_or_else(||bad("no video"))?;
    if videos.next().is_some(){return Err(bad("requires exactly one non-attached video stream"))}
    let color_basis=source_color(v,assume_sdr)?;
    let index=v.index.ok_or_else(||bad("missing stream index"))?;
    let length=v.duration_us.or(m.duration_us).ok_or_else(||bad("unknown duration"))?;
    if length<30_000_000{return Err(bad("choose a video at least 30 seconds long"))}
    let h=v.height.ok_or_else(||bad("unknown height"))?;let width=v.width.ok_or_else(||bad("unknown width"))?;
    if h>2160||width>3840||h%2!=0||width%2!=0{return Err(bad("supports even dimensions up to 3840x2160"))}
    let bitrate=match h{0..=720=>2_500_000,721..=1080=>5_000_000,_=>18_000_000};
    let temp=Temp::new()?;let mut results=Vec::new();
    let routes=[Route{encoder:"libx264",preset:"fast",codec:"h264",pix:"yuv420p"},Route{encoder:"hevc_nvenc",preset:"p4",codec:"hevc",pix:"nv12"}];
    for (i,route) in routes.into_iter().enumerate(){
        if i==1&&!report.capabilities.iter().any(|c|c.encoder==route.encoder&&c.bit_depth==8&&c.evidence==hardware::Evidence::Verified){
            results.push(json!({"encoder":route.encoder,"status":"skipped","reason":"hardware_route_not_verified"}));continue
        }
        eprintln!("S03: {} samples then held-out window",route.encoder);
        let run=|offset,dur,j:usize|measure(ffmpeg,ffprobe,&m.path,index,route,bitrate,offset,dur,&temp.0.join(format!("{}.mkv",i*3+j)),cancel);
        let (a,ra)=run(0,4,0)?;let (b,rb)=run(24,4,1)?;
        let predicted=estimate(&[a,b],12.0).ok_or_else(||bad("invalid samples"))?;
        let (actual,rv)=run(8,12,2)?;
        let p=predicted["estimated_video_seconds"].as_f64().ok_or_else(||bad("invalid prediction"))?;
        let lo=predicted["heuristic_seconds_range"][0].as_f64().unwrap_or(f64::NAN);
        let hi=predicted["heuristic_seconds_range"][1].as_f64().unwrap_or(f64::NAN);
        results.push(json!({"encoder":route.encoder,"preset":route.preset,"status":"ok","samples":[ra,rb],"prediction":predicted,"holdout":rv,"absolute_relative_error_pct":(p/actual.wall_seconds-1.0).abs()*100.0,"covered":actual.wall_seconds>=lo&&actual.wall_seconds<=hi}));
    }
    Ok(json!({"schema_version":1,"model_version":VERSION,"kind":"real_source_disjoint_window_validation","source":{"codec":v.codec,"width":width,"height":h,"bit_depth":v.bit_depth,"duration_us":length,"hdr_classification":format!("{:?}",v.hdr),"color_basis":color_basis,"color_transfer":v.color_transfer,"color_primaries":v.color_primaries,"color_space":v.color_space,"pixel_format":v.pixel_format},"target_bitrate_bps":bitrate,"scope":"software decode, format, encode and Matroska video mux; no audio; no production ETA or isolated GPU throughput","system_snapshot":crate::benchmark::system_snapshot(),"results":results}))
}
#[cfg(test)]
mod tests{
    use super::*;
    #[test] fn untagged_sdr_is_explicit_and_test_only(){
        fn stream(extra:&str)->media::Stream{
            let raw=format!(r#"{{"format":{{}},"streams":[{{"codec_type":"video","codec_name":"h264","pix_fmt":"yuv420p","color_space":"bt709"{extra}}}]}}"#);
            media::parse("test.mp4".into(),raw.as_bytes()).unwrap().streams.remove(0)
        }
        let v=stream("");assert_eq!(v.hdr,media::Hdr::Unknown);
        assert!(source_color(&v,false).is_err());assert_eq!(source_color(&v,true).unwrap(),"user_confirmed_untagged_sdr");
        assert_eq!(source_color(&stream(r#","color_transfer":"bt709""#),false).unwrap(),"metadata_sdr");
        for extra in [r#","color_transfer":"smpte2084""#,r#","color_transfer":"arib-std-b67""#,r#","color_transfer":"linear""#,r#","color_primaries":"bt2020""#,r#","side_data_list":[{"side_data_type":"Mastering display metadata"}]"#,r#","side_data_list":[{"side_data_type":"DOVI configuration record"}]"#]{
            assert!(source_color(&stream(extra),true).is_err(),"{extra}");
        }
        let mut deep=v.clone();deep.bit_depth=Some(10);assert!(source_color(&deep,true).is_err());
    }
    #[test] fn projection_and_limits(){
        let a=Sample{source_seconds:4.0,wall_seconds:2.0};let b=Sample{source_seconds:4.0,wall_seconds:4.0};
        let v=estimate(&[a,b],12.0).unwrap();assert_eq!(v["estimated_video_seconds"],9.0);assert_eq!(v["heuristic_seconds_range"],json!([3.0,18.0]));assert!(v["production_seconds"].is_null());
        assert!(estimate(&[a],12.0).is_none());assert!(estimate(&[a,b],f64::INFINITY).is_none());assert!(estimate(&[a,Sample{source_seconds:0.0,wall_seconds:1.0}],12.0).is_none());
    }
}
