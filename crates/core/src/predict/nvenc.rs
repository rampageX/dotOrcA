//! External SDK reference only; never combined with S02 wall-clock FPS.
use super::{aliases,Database};
use serde_json::{json,Value};

pub const VERSION:&str="nvenc-sdk13.1-v1";
pub struct NvencQuery<'a>{
    pub gpu_name:&'a str,pub codec:&'a str,pub preset:u8,
    pub width:u32,pub height:u32,pub bit_depth:u8,pub chroma:&'a str,
    pub rate_control:&'a str,pub tuning:&'a str,
    /// Video engine clock, not graphics or memory clock; caller supplies observed MHz.
    pub video_clock_mhz:Option<u32>,
}
fn unknown(reason:&str)->Value{
    json!({"model_version":VERSION,"kind":"unknown","reason":reason,"confidence":"none","estimated_per_engine_fps":null,"pipeline_fps":null,"production_seconds":null})
}
impl Database{
    pub fn nvenc_reference(&self,q:&NvencQuery<'_>)->Value{
        if q.width!=1920||q.height!=1080||q.bit_depth!=8||q.chroma!="420"||q.rate_control!="vbr"||q.tuning!="hq"||!(1..=7).contains(&q.preset){return unknown("outside_vendor_reference_conditions")}
        let Some(gpu)=aliases(&self.mapping,"gpus",q.gpu_name)else{return unknown("unknown_gpu")};
        // SDK performance anchors are desktop devices. Do not transfer to mobile or a different NVENC generation.
        if gpu["mobile"]!=false{return unknown("mobile_not_calibrated")}
        let Some(anchor)=self.nvenc["anchors"].as_array().and_then(|xs|xs.iter().find(|a|a["architecture"]==gpu["architecture"]&&a["generation"]==gpu["nvenc_generation"]))else{return unknown("no_matching_engine_generation")};
        let Some(fps)=anchor.get(q.codec).and_then(Value::as_array)else{return unknown("codec_not_in_vendor_reference")};
        let i=((q.preset-1)/2) as usize;
        let Some(lo)=fps.get(i).and_then(Value::as_f64)else{return unknown("invalid_reference")};
        let interpolated=q.preset%2==0;
        let value=if interpolated{let Some(hi)=fps.get(i+1).and_then(Value::as_f64)else{return unknown("invalid_reference")};(lo*hi).sqrt()}else{lo};
        let Some(clock)=anchor["clock_mhz"].as_f64()else{return unknown("invalid_reference")};
        let ratio=q.video_clock_mhz.map(|c|c as f64/clock).filter(|r|(0.5..=1.5).contains(r));
        let estimate=ratio.map(|r|value*r);
        json!({"model_version":VERSION,"kind":if estimate.is_some(){"inferred_vendor_per_engine"}else{"vendor_reference_only"},"confidence":if estimate.is_some(){"low_unvalidated_proxy"}else{"none_for_target"},"source":self.nvenc["source"],"anchor_device":anchor["device"],"anchor_clock_mhz":clock,"target_video_clock_mhz":q.video_clock_mhz,"anchor_per_engine_fps":value,"anchor_value_kind":if interpolated{"interpolated_preset"}else{"vendor_measured"},"estimated_per_engine_fps":estimate,"engine_count":gpu["nvenc_engines"],"pipeline_fps":null,"production_seconds":null,"reason":"SDK per-engine reference; no device capability certification, no engine-count multiplier; not dotOrcA measurement"})
    }
}
#[cfg(test)]
mod tests{
    use super::*;
    #[test]fn vendor_reference_is_not_pipeline(){
        let db=Database::bundled().unwrap();
        let mut q=NvencQuery{gpu_name:"NVIDIA GeForce RTX 4080",codec:"h264",preset:3,width:1920,height:1080,bit_depth:8,chroma:"420",rate_control:"vbr",tuning:"hq",video_clock_mhz:None};
        let x=db.nvenc_reference(&q);assert_eq!(x["anchor_per_engine_fps"],647.0);assert_eq!(x["anchor_value_kind"],"vendor_measured");assert!(x["estimated_per_engine_fps"].is_null());
        q.video_clock_mhz=Some(2415);let x=db.nvenc_reference(&q);assert_eq!(x["estimated_per_engine_fps"],647.0);assert_eq!(x["engine_count"],2);assert!(x["pipeline_fps"].is_null());assert!(x["production_seconds"].is_null());
        q.preset=4;let x=db.nvenc_reference(&q);assert_eq!(x["anchor_value_kind"],"interpolated_preset");assert!(x["estimated_per_engine_fps"].as_f64().unwrap()<647.0);
        q.bit_depth=10;assert_eq!(db.nvenc_reference(&q)["kind"],"unknown");q.bit_depth=8;
        q.gpu_name="NVIDIA T600";assert_eq!(db.nvenc_reference(&q)["reason"],"no_matching_engine_generation");
        q.gpu_name="NVIDIA T600 Laptop GPU";assert_eq!(db.nvenc_reference(&q)["reason"],"mobile_not_calibrated");
        q.gpu_name="unknown";assert_eq!(db.nvenc_reference(&q)["kind"],"unknown");
    }
}
