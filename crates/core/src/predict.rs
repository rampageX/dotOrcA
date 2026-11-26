//! S03 reference-pipeline estimator. Never an isolated GPU encoder benchmark.
use serde_json::{Value,json};
use std::{collections::HashSet,fs,io,path::Path};
use crate::hardware::{Inventory,Capability,Evidence};

pub const MODEL_VERSION:&str="reference-pipeline-v1";
const OFFICIAL:&str=include_str!("../../../data/official/s02-physical-v1.json");
const MAP:&str=include_str!("../../../data/official/hardware-map-v1.json");
const MAX_BYTES:u64=16*1024*1024;
fn bad(s:&str)->io::Error{io::Error::new(io::ErrorKind::InvalidData,s)}
fn arr<'a>(v:&'a Value,k:&str)->io::Result<&'a Vec<Value>>{v[k].as_array().ok_or_else(||bad(k))}
fn string<'a>(v:&'a Value,k:&str)->io::Result<&'a str>{v[k].as_str().filter(|s|!s.is_empty()).ok_or_else(||bad(k))}
fn uint(v:&Value,k:&str)->io::Result<u64>{v[k].as_u64().filter(|n|*n>0).ok_or_else(||bad(k))}
fn positive(v:&Value,k:&str)->io::Result<f64>{v[k].as_f64().filter(|x|x.is_finite()&&*x>0.0).ok_or_else(||bad(k))}
fn hash(s:&str)->bool{s.len()==64&&s.bytes().all(|b|b.is_ascii_hexdigit())}
fn near(a:f64,b:f64)->bool{(a-b).abs()<=1e-6*a.abs().max(b.abs()).max(1.0)}
fn read(path:&Path)->io::Result<String>{
    use std::io::Read;
    let f=fs::File::open(path)?;
    let mut s=String::new();
    f.take(MAX_BYTES+1).read_to_string(&mut s)?;
    if s.len() as u64>MAX_BYTES{return Err(bad("dataset too large"))}
    Ok(s)
}
fn parse(text:&str,origin:&str)->io::Result<Vec<Value>>{
    let v:Value=serde_json::from_str(text).map_err(|e|bad(&e.to_string()))?;
    let ds=v.as_array().ok_or_else(||bad("dataset array"))?;
    let mut sources=HashSet::new();
    for d in ds {
        if d["schema_version"]!=1||d["origin"]!=origin||d["kind"]!="measured_pipeline"||d["matrix_version"]!="synthetic-sdr-v1"||d["metric_version"]!="ffmpeg-ssim-psnr-v1"{
            return Err(bad("schema/origin/workload mismatch"));
        }
        string(d,"dataset_version")?;
        let sha=string(d,"source_report_sha256")?;
        if !hash(sha)||!sources.insert(sha){return Err(bad("invalid/duplicate source hash"))}
        let e=&d["environment"];
        string(e,"cpu")?;uint(e,"logical_cpus")?;
        for k in ["ffmpeg_sha256","ffprobe_sha256"] {if !hash(string(e,k)?){return Err(bad(k))}}
        if e["decode"]!="software"||e["profile"]!="standard"||e["repeats"]!=1||e["os"]!="windows"||e["arch"]!="x86_64"{return Err(bad("unsupported benchmark environment"))}
        for a in arr(e,"adapters")?{string(a,"name")?;string(a,"driver_version")?;}
        let fixtures=arr(d,"fixtures")?;let records=arr(d,"records")?;
        if fixtures.len()!=12||records.is_empty()||uint(d,"source_cases")?!=288||d["source_skipped"].as_u64().and_then(|n|n.checked_add(records.len() as u64))!=Some(288){return Err(bad("incomplete source matrix"))}
        let mut fixture_ids=HashSet::new();
        for f in fixtures{
            let (w,h,b)=(uint(f,"width")?,uint(f,"height")?,uint(f,"bit_depth")?);
            let bitrate=match(w,h){(1280,720)=>2500000,(1920,1080)=>5000000,(3840,2160)=>18000000,_=>return Err(bad("fixture resolution"))};
            let id=string(f,"id")?;
            if !matches!(b,8|10)||!["motion","noise"].iter().any(|s|id==format!("{w}x{h}-{b}bit-{s}"))||!fixture_ids.insert(id)||f["frames"]!=180||f["fps"]["numerator"]!=30||f["fps"]["denominator"]!=1||f["target_bitrate_bps"]!=bitrate||!hash(string(f,"decoded_sha256")?)||!hash(string(f,"file_sha256")?)||!near(positive(f,"decode_baseline_fps")?*positive(f,"decode_baseline_wall_seconds")?,180.0){return Err(bad("fixture contract"))}
        }
        let mut cases=HashSet::new();
        for r in records{
            let id=string(r,"case_id")?;
            let fid=string(r,"fixture_id")?;
            let f=fixtures.iter().find(|f|f["id"]==fid).ok_or_else(||bad("unknown fixture"))?;
            let encoder=string(r,"encoder")?;let preset=string(r,"preset")?;
            let (codec,presets):(&str,&[&str])=match encoder {
                "libx264"=>("h264",&["fast","medium"]),
                "libx265"=>("hevc",&["fast","medium"]),
                "libsvtav1"=>("av1",&["6","8"]),
                "h264_nvenc"=>("h264",&["p4","p6"]),
                "hevc_nvenc"=>("hevc",&["p4","p6"]),
                "av1_nvenc"=>("av1",&["p4","p6"]),
                "h264_qsv"=>("h264",&["fast","slow"]),
                "hevc_qsv"=>("hevc",&["fast","slow"]),
                "av1_qsv"=>("av1",&["fast","slow"]),
                "h264_amf"=>("h264",&["speed","quality"]),
                "hevc_amf"=>("hevc",&["speed","quality"]),
                "av1_amf"=>("av1",&["speed","quality"]),
                _=>return Err(bad("encoder"))
            };
            if !presets.contains(&preset)||r["codec"]!=codec||id!=format!("{fid}-{encoder}-{preset}-1")||!cases.insert(id)||r["repeat"]!=1||r["encoded_frames"]!=180||r["bit_depth"]!=f["bit_depth"]||r["target_bitrate_bps"]!=f["target_bitrate_bps"]||!near(positive(r,"fps")?*positive(r,"encode_wall_seconds")?,180.0)||!near(positive(r,"actual_container_bitrate_bps")?,uint(r,"output_bytes")? as f64*8.0/6.0)||!hash(string(r,"output_sha256")?)||!r["ssim"].as_f64().is_some_and(|x|(0.0..=1.0).contains(&x))||!(r["psnr_db"].as_f64().is_some_and(|x|x.is_finite()&&x>=0.0)||r["psnr_db"]=="inf"){return Err(bad("invalid sample"))}
            let argv=arr(r,"argv")?;
            if argv.last().and_then(Value::as_str)!=Some("<output>")||!argv.windows(2).any(|p|p[0]=="-i"&&p[1]=="<input>"){return Err(bad("argv not sanitized"))}
        }
    }
    Ok(ds.clone())
}
#[derive(Debug)]
pub struct Database{official:Vec<Value>,local:Vec<Value>,mapping:Value}
#[derive(Debug,Clone)]
pub struct Query<'a>{
    pub inventory:&'a Inventory,
    pub capabilities:&'a [Capability],
    pub ffmpeg_sha256:&'a str,
    pub ffprobe_sha256:&'a str,
    pub encoder:&'a str,pub preset:&'a str,
    pub width:u32,pub height:u32,pub bit_depth:u8,pub target_bitrate_bps:u64,
    /// Only callers with exact S02 reference provenance may use a fixture hash.
    /// None = real/unknown source: no production estimate inferred from synthetic FFV1.
    pub fixture_decoded_sha256:Option<&'a str>,
    pub frames:u64,
}
fn aliases<'a>(map:&'a Value,key:&str,name:&str)->Option<&'a Value>{
    map[key].as_array()?.iter().find(|v|v["aliases"].as_array().is_some_and(|a|a.iter().any(|x|x.as_str().is_some_and(|s|s.eq_ignore_ascii_case(name.trim())))))
}
fn matching_cpu(inv:&Inventory,e:&Value)->bool{
    inv.cpu.as_deref()==e["cpu"].as_str()&&inv.logical_cpus.map(|n|n as u64)==e["logical_cpus"].as_u64()
}
fn matching_adapters(inv:&Inventory,e:&Value)->bool{
    let Some(a)=e["adapters"].as_array() else{return false};
    inv.adapter_scan_complete&&a.len()==inv.adapters.len()&&a.iter().all(|v|inv.adapters.iter().any(|x|x.name.as_deref()==v["name"].as_str()&&x.driver_version.as_deref()==v["driver_version"].as_str()))
}
fn software(e:&str)->bool{matches!(e,"libx264"|"libx265"|"libsvtav1")}
fn unknown(reason:&str)->Value{json!({"model_version":MODEL_VERSION,"kind":"unknown","confidence":"none","reason":reason,"pipeline_fps":null,"reference_seconds":null,"production_seconds":null,"isolated_encoder_fps":null})}
impl Database{
    pub fn bundled()->io::Result<Self>{
        let official=parse(OFFICIAL,"official")?;
        let mapping:Value=serde_json::from_str(MAP).map_err(|e|bad(&e.to_string()))?;
        if mapping["schema_version"]!=1||mapping["version"]!="hardware-map-v1"||official.len()!=2||official.iter().map(|d|d["records"].as_array().map_or(0,Vec::len)).sum::<usize>()!=264{return Err(bad("bundled database integrity"))}
        Ok(Self{official,local:Vec::new(),mapping})
    }
    /// Reads a separate, explicit local file. No write, merge, download or learning.
    pub fn with_local(mut self,path:&Path)->io::Result<Self>{
        let local=parse(&read(path)?,"local")?;
        if local.iter().any(|d|self.official.iter().any(|o|o["source_report_sha256"]==d["source_report_sha256"])){return Err(bad("duplicate official/local observation"))}
        self.local=local;Ok(self)
    }
    pub fn info(&self)->Value{
        json!({"model_version":MODEL_VERSION,"official_datasets":self.official.len(),"local_datasets":self.local.len(),"official_samples":self.official.iter().map(|d|d["records"].as_array().map_or(0,Vec::len)).sum::<usize>(),"local_samples":self.local.iter().map(|d|d["records"].as_array().map_or(0,Vec::len)).sum::<usize>(),"mapping":self.mapping})
    }
    pub fn hardware_mapping(&self,inv:&Inventory)->Value{
        json!({"cpu":inv.cpu.as_deref().and_then(|n|aliases(&self.mapping,"cpus",n)),"gpus":inv.adapters.iter().map(|a|json!({"name":a.name,"mapping":a.name.as_deref().and_then(|n|aliases(&self.mapping,"gpus",n))})).collect::<Vec<_>>()})
    }
    pub fn estimate(&self,q:&Query<'_>)->Value{
        if q.frames==0||q.width==0||q.height==0||q.target_bitrate_bps==0||!matches!(q.bit_depth,8|10){return unknown("invalid_query")}
        let Some(fixture_hash)=q.fixture_decoded_sha256.filter(|s|hash(s))else{return unknown("real_or_unknown_source_not_calibrated")};
        if !hash(q.ffmpeg_sha256)||!hash(q.ffprobe_sha256){return unknown("tool_fingerprint_unknown")}
        let soft=software(q.encoder);
        if !soft&&!q.capabilities.iter().any(|c|c.encoder==q.encoder&&c.bit_depth==q.bit_depth&&c.evidence==Evidence::Verified){return unknown("hardware_route_not_verified")}
        let mut exact=Vec::new();let mut mapped=Vec::new();
        for d in self.local.iter().chain(&self.official){
            let e=&d["environment"];
            if e["ffmpeg_sha256"]!=q.ffmpeg_sha256||e["ffprobe_sha256"]!=q.ffprobe_sha256{continue}
            let Some(f)=d["fixtures"].as_array().and_then(|fs|fs.iter().find(|f|f["decoded_sha256"]==fixture_hash&&f["width"]==q.width&&f["height"]==q.height&&f["bit_depth"]==q.bit_depth&&f["target_bitrate_bps"]==q.target_bitrate_bps))else{continue};
            for r in d["records"].as_array().into_iter().flatten().filter(|r|r["fixture_id"]==f["id"]&&r["encoder"]==q.encoder&&r["preset"]==q.preset){
                let fps=r["fps"].as_f64().unwrap_or(0.0);
                let evidence=json!({"origin":d["origin"],"dataset_version":d["dataset_version"],"source_report_sha256":d["source_report_sha256"],"case_id":r["case_id"],"observed_pipeline_fps":fps,"decode_only_fps":f["decode_baseline_fps"]});
                if matching_cpu(q.inventory,e)&&matching_adapters(q.inventory,e){exact.push((fps,evidence));continue}
                if !soft{continue} // GPU transfer cannot separate software decode / encode / transport.
                let Some(target)=q.inventory.cpu.as_deref().and_then(|n|aliases(&self.mapping,"cpus",n))else{continue};
                let Some(anchor)=e["cpu"].as_str().and_then(|n|aliases(&self.mapping,"cpus",n))else{continue};
                if target["id"]==anchor["id"]||q.inventory.logical_cpus.map(|n|n as u64)!=target["threads"].as_u64(){continue}
                let Some((s,m))=target["single"].as_f64().zip(target["multi"].as_f64()).zip(anchor["single"].as_f64().zip(anchor["multi"].as_f64())).map(|((s,m),(a,b))|(s/a,m/b))else{continue};
                if !(0.5..=2.0).contains(&s)||!(0.5..=2.0).contains(&m){continue}
                // Unfitted proxy: geometric interpolation, deliberately bounded by both index ratios.
                // Extra 0.5x safety margin is a heuristic, NOT an empirical confidence interval.
                mapped.push((fps*s.min(m)*0.5,fps*(s*m).sqrt(),fps*s.max(m),evidence));
            }
        }
        if !exact.is_empty(){
            let low=exact.iter().map(|x|x.0).fold(f64::INFINITY,f64::min);
            let high=exact.iter().map(|x|x.0).fold(0.0,f64::max);
            return json!({"model_version":MODEL_VERSION,"kind":"measured_reference","confidence":"limited_single_run","pipeline_fps":low,"observed_fps_range":[low,high],"reference_seconds":q.frames as f64/low,"production_seconds":null,"isolated_encoder_fps":null,"evidence":exact.into_iter().map(|x|x.1).collect::<Vec<_>>(),"reason":"wall_clock_ffv1_decode_filter_mux; duration projection unvalidated"});
        }
        if !mapped.is_empty(){
            let low=mapped.iter().map(|x|x.0).fold(f64::INFINITY,f64::min);
            let high=mapped.iter().map(|x|x.2).fold(0.0,f64::max);
            let point=mapped.iter().map(|x|x.1).fold(f64::INFINITY,f64::min);
            return json!({"model_version":MODEL_VERSION,"kind":"inferred_cpu_reference","confidence":"low_unvalidated_proxy","pipeline_fps":point,"heuristic_fps_range":[low,high],"reference_seconds":q.frames as f64/point,"conservative_reference_seconds":q.frames as f64/low,"production_seconds":null,"isolated_encoder_fps":null,"evidence":mapped.into_iter().map(|x|x.3).collect::<Vec<_>>(),"reason":"public single/multi index geometric blend; no calibrated interval"});
        }
        unknown(if soft{"no_comparable_cpu_reference"}else{"no_comparable_gpu_pipeline_reference"})
    }
    pub fn reference_cases(&self)->impl Iterator<Item=(&Value,&Value,&Value)>{
        self.official.iter().flat_map(|d|d["records"].as_array().into_iter().flatten().filter_map(move |r|d["fixtures"].as_array()?.iter().find(|f|f["id"]==r["fixture_id"]).map(|f|(d,f,r))))
    }
}
#[cfg(test)]
mod tests{
    use super::*;
    fn inv(e:&Value)->Inventory{Inventory{cpu:e["cpu"].as_str().map(str::to_owned),logical_cpus:e["logical_cpus"].as_u64().map(|n|n as usize),adapters:e["adapters"].as_array().unwrap().iter().map(|a|crate::hardware::Adapter{name:a["name"].as_str().map(str::to_owned),driver_version:a["driver_version"].as_str().map(str::to_owned),instance_id:None}).collect(),adapter_scan_complete:true,adapter_scan_error:None}}
    #[test]fn observations_and_queries(){
        let db=Database::bundled().unwrap();assert_eq!(db.info()["official_samples"],264);
        let mut n=0;
        for(d,f,r)in db.reference_cases(){
            let e=&d["environment"];let hw=inv(e);
            let cap=[Capability{encoder:r["encoder"].as_str().unwrap().into(),bit_depth:r["bit_depth"].as_u64().unwrap() as u8,evidence:Evidence::Verified,diagnostic:String::new()}];
            let mut q=Query{inventory:&hw,capabilities:&cap,ffmpeg_sha256:e["ffmpeg_sha256"].as_str().unwrap(),ffprobe_sha256:e["ffprobe_sha256"].as_str().unwrap(),encoder:&cap[0].encoder,preset:r["preset"].as_str().unwrap(),width:f["width"].as_u64().unwrap() as u32,height:f["height"].as_u64().unwrap() as u32,bit_depth:cap[0].bit_depth,target_bitrate_bps:f["target_bitrate_bps"].as_u64().unwrap(),fixture_decoded_sha256:f["decoded_sha256"].as_str(),frames:180};
            let x=db.estimate(&q);assert_eq!(x["kind"],"measured_reference");assert!(near(x["pipeline_fps"].as_f64().unwrap(),r["fps"].as_f64().unwrap()));assert!(x["isolated_encoder_fps"].is_null());assert!(x["production_seconds"].is_null());
            q.fixture_decoded_sha256=None;assert_eq!(db.estimate(&q)["kind"],"unknown");
            q.fixture_decoded_sha256=f["decoded_sha256"].as_str();q.ffmpeg_sha256="wrong";assert_eq!(db.estimate(&q)["kind"],"unknown");
            q.ffmpeg_sha256=e["ffmpeg_sha256"].as_str().unwrap();q.capabilities=&[];
            if !software(q.encoder){assert_eq!(db.estimate(&q)["kind"],"unknown")}
            // Scope the borrow before testing a deliberately unknown CPU.
            let mut other=hw.clone();other.cpu=Some("unknown model".into());q.inventory=&other;
            assert_eq!(db.estimate(&q)["kind"],"unknown");n+=1;
        }assert_eq!(n,264)
    }
    #[test]fn rejects_invalid_data(){
        assert!(parse(OFFICIAL,"local").is_err());
        let mut v:Value=serde_json::from_str(OFFICIAL).unwrap();v[0]["records"][0]["fps"]=json!(0);
        assert!(parse(&v.to_string(),"official").is_err());
        assert!(parse("[]","official").unwrap().is_empty());
    }
}
