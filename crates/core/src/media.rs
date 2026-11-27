//! FFprobe stream/format metadata. Unknown is not zero or SDR.
use crate::process::{self,Request,Stop};
use serde_json::Value;
use std::{ffi::OsString, io, path::{Path,PathBuf}, sync::atomic::AtomicBool};

#[derive(Debug,Clone,Copy,PartialEq,Eq)]
pub struct Rational { pub numerator:u64,pub denominator:u64 }
impl Rational {
    fn parse(s:&str)->Option<Self> {
        let (n,d)=s.split_once('/')?;
        let (n,d)=(n.parse::<u64>().ok()?,d.parse::<u64>().ok()?);
        (n>0&&d>0).then_some(Self{numerator:n,denominator:d})
    }
}
#[derive(Debug,Clone,Copy,PartialEq,Eq)]
pub enum Hdr { Pq,Hlg,DolbyVision,Sdr,Unknown }
impl Hdr { pub fn label(self)->&'static str {match self{Self::Pq=>"PQ",Self::Hlg=>"HLG",Self::DolbyVision=>"Dolby Vision",Self::Sdr=>"SDR",Self::Unknown=>"unknown"}} }
#[derive(Debug,Clone)]
pub struct Stream {
    pub index: Option<u64>, pub kind: Option<String>, pub codec: Option<String>,
    pub width:Option<u64>,pub height:Option<u64>,pub avg_rate:Option<Rational>,pub nominal_rate:Option<Rational>,
    pub duration_us:Option<u64>,pub bit_depth:Option<u64>,pub pixel_format:Option<String>,
    pub color_range:Option<String>,pub color_space:Option<String>,pub color_transfer:Option<String>,pub color_primaries:Option<String>,
    pub hdr:Hdr,pub attached_picture:bool,pub channels:Option<u64>,pub sample_rate:Option<u64>,
    pub raw:Value,
}
#[derive(Debug,Clone)]
pub struct MediaInfo {
    pub path:PathBuf,pub size_bytes:Option<u64>,pub duration_us:Option<u64>,
    pub format:Option<String>,pub streams:Vec<Stream>,pub raw:Value,
}
fn string(v:&Value,k:&str)->Option<String> {
    v.get(k)?.as_str().filter(|s|!s.is_empty()&&*s!="N/A"&&*s!="unknown"&&*s!="unspecified").map(str::to_owned)
}
fn integer(v:&Value,k:&str)->Option<u64> {
    let x=v.get(k)?; x.as_u64().or_else(||x.as_str()?.parse().ok())
}
fn duration(v:&Value)->Option<u64> {
    let s=v.as_str().map(str::to_owned).or_else(||v.as_number().map(ToString::to_string))?;
    let (whole,fraction)=s.split_once('.').unwrap_or((&s,""));
    if whole.is_empty()||!whole.bytes().all(|c|c.is_ascii_digit())||!fraction.bytes().all(|c|c.is_ascii_digit()) {return None;}
    let mut us=0u64;
    for (i,c) in fraction.bytes().take(6).enumerate() {us+=(c-b'0') as u64*10u64.pow(5-i as u32);}
    whole.parse::<u64>().ok()?.checked_mul(1_000_000)?.checked_add(us)
}
fn depth(v:&Value)->Option<u64> {
    // bits_per_raw_sample can be absent or 0, especially for hardware-decoded video.
    integer(v,"bits_per_raw_sample").filter(|n|*n>0).or_else(||integer(v,"bits_per_sample").filter(|n|*n>0)).or_else(||{
        let p=v.get("pix_fmt")?.as_str()?;
        let p=p.strip_suffix("le").or_else(||p.strip_suffix("be")).unwrap_or(p);
        match p {
            "yuv420p"|"yuv422p"|"yuv444p"|"yuvj420p"|"yuvj422p"|"yuvj444p"|"nv12"|"nv21"|"rgb24"|"bgr24"|"rgba"|"bgra"|"gray"=>Some(8),
            "p010"|"p210"|"x2rgb10"|"x2bgr10"=>Some(10),
            "p012"|"p212"=>Some(12), "p016"|"p216"|"gray16"=>Some(16),
            _ if p.starts_with("yuv")||p.starts_with("gbrp")||p.starts_with("gray") => [9,10,12,14,16].into_iter().find(|n|p.ends_with(&n.to_string())),
            _=>None,
        }
    })
}
fn hdr(v:&Value)->Hdr {
    if v.get("side_data_list").and_then(Value::as_array).is_some_and(|a|a.iter().any(|x|x.get("side_data_type").and_then(Value::as_str).is_some_and(|s|s.contains("DOVI")||s.contains("Dolby Vision")))) { return Hdr::DolbyVision; }
    match v.get("color_transfer").and_then(Value::as_str) {
        Some("smpte2084")=>Hdr::Pq,Some("arib-std-b67")=>Hdr::Hlg,
        Some("bt709"|"smpte170m"|"bt470m"|"bt470bg"|"smpte240m"|"iec61966-2-1"|"bt2020-10"|"bt2020-12")=>Hdr::Sdr,
        _=>Hdr::Unknown,
    }
}
pub fn parse(path:PathBuf,json:&[u8])->io::Result<MediaInfo> {
    let raw:Value=serde_json::from_slice(json).map_err(|e|io::Error::new(io::ErrorKind::InvalidData,e))?;
    let streams=raw.get("streams").and_then(Value::as_array).ok_or_else(||io::Error::new(io::ErrorKind::InvalidData,crate::text::en(crate::text::Msg::MissingStreams)))?;
    let f=raw.get("format").filter(|v|v.is_object()).ok_or_else(||io::Error::new(io::ErrorKind::InvalidData,crate::text::en(crate::text::Msg::MissingFormat)))?;
    let mut parsed=Vec::with_capacity(streams.len());
    for v in streams {
        if !v.is_object(){return Err(io::Error::new(io::ErrorKind::InvalidData,crate::text::en(crate::text::Msg::InvalidStream)));}
        parsed.push(Stream{
            index:integer(v,"index"),kind:string(v,"codec_type"),codec:string(v,"codec_name"),
            width:integer(v,"width").filter(|n|*n>0),height:integer(v,"height").filter(|n|*n>0),
            avg_rate:string(v,"avg_frame_rate").and_then(|s|Rational::parse(&s)),nominal_rate:string(v,"r_frame_rate").and_then(|s|Rational::parse(&s)),
            duration_us:v.get("duration").and_then(duration),bit_depth:depth(v),pixel_format:string(v,"pix_fmt"),
            color_range:string(v,"color_range"),color_space:string(v,"color_space"),color_transfer:string(v,"color_transfer"),color_primaries:string(v,"color_primaries"),
            hdr:hdr(v),attached_picture:v.pointer("/disposition/attached_pic").and_then(Value::as_u64)==Some(1),
            channels:integer(v,"channels"),sample_rate:integer(v,"sample_rate"),raw:v.clone(),
        });
    }
    Ok(MediaInfo{path,size_bytes:integer(f,"size"),duration_us:f.get("duration").and_then(duration),format:string(f,"format_name"),streams:parsed,raw})
}
pub fn probe(ffprobe:&Path,path:&Path,cancel:&AtomicBool)->io::Result<MediaInfo> {
    // Canonicalize without converting the native path to UTF-8 or shell escaping.
    // Absolute paths prevent leading-hyphen options from being interpreted as flags.
    let path=std::fs::canonicalize(path)?;
    let meta=std::fs::metadata(&path)?;
    if !meta.is_file(){return Err(io::Error::new(io::ErrorKind::InvalidInput,crate::text::en(crate::text::Msg::NotRegular)));}
    let req=Request::new(ffprobe).args(["-v","error","-show_format","-show_streams","-of","json","-i"]).args([OsString::from(&path)]);
    let result=process::run(&req,cancel)?;
    if result.stop!=Stop::Exited || !result.status.success() {
        return Err(io::Error::other(crate::message!(ProbeError,format!("{:?}",result.stop),result.status,String::from_utf8_lossy(&result.stderr.bytes))));
    }
    if result.stdout.discarded>0 {return Err(io::Error::new(io::ErrorKind::InvalidData,crate::text::en(crate::text::Msg::ProbeLimit)));}
    let mut info=parse(path,&result.stdout.bytes)?;
    info.size_bytes=Some(meta.len());
    Ok(info)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn exact_metadata_and_unknowns(){
        let data=br#"{"format":{"size":"9007199254740993","duration":"123456.1234567"},"streams":[{"index":0,"codec_type":"video","codec_name":"hevc","pix_fmt":"yuv420p10le","avg_frame_rate":"30000/1001","color_transfer":"smpte2084","color_primaries":"bt2020","side_data_list":[{"side_data_type":"Mastering display metadata"}]},{"codec_type":"audio","avg_frame_rate":"0/0"},{"codec_type":"video","color_transfer":"arib-std-b67"},{"codec_type":"video","side_data_list":[{"side_data_type":"DOVI configuration record"}]}]}"#;
        let m=parse("x".into(),data).unwrap();
        assert_eq!(m.size_bytes,Some(9007199254740993)); assert_eq!(m.duration_us,Some(123456123456));
        assert_eq!(m.streams[0].avg_rate,Some(Rational{numerator:30000,denominator:1001}));
        assert_eq!(m.streams[0].bit_depth,Some(10)); assert_eq!(m.streams[0].hdr,Hdr::Pq);
        assert_eq!(m.streams[1].avg_rate,None);assert_eq!(m.streams[1].hdr,Hdr::Unknown);
        assert_eq!(m.streams[2].hdr,Hdr::Hlg);assert_eq!(m.streams[3].hdr,Hdr::DolbyVision);
        assert!(parse("x".into(),b"{}").is_err());
    }
}
