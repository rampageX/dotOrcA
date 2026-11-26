//! Capability evidence, not encoder policy. No throughput prediction.
use crate::process::{self,Request,Stop};
use std::{io,path::{Path,PathBuf},sync::atomic::{AtomicBool,Ordering},time::{Duration,Instant,SystemTime}};
#[cfg(windows)]
#[allow(unsafe_code)]
mod windows;
#[derive(Debug,Clone,PartialEq,Eq)]
pub struct Adapter {pub name:Option<String>,pub instance_id:Option<String>,pub driver_version:Option<String>}
#[derive(Debug,Clone,PartialEq,Eq)]
pub struct Inventory {pub cpu:Option<String>,pub logical_cpus:Option<usize>,pub adapters:Vec<Adapter>,pub adapter_scan_complete:bool,pub adapter_scan_error:Option<String>}
pub fn inventory()->Inventory {
    #[cfg(windows)] let (adapters,adapter_scan_complete,adapter_scan_error)=match windows::adapters(){Ok(a)=>(a,true,None),Err(e)=>(Vec::new(),false,Some(e.to_string()))};
    #[cfg(not(windows))] let (adapters,adapter_scan_complete,adapter_scan_error)=(Vec::new(),false,Some("Windows adapter inventory unavailable on this platform".into()));
    #[cfg(windows)] let cpu=windows::cpu_name().or_else(||std::env::var("PROCESSOR_IDENTIFIER").ok());
    #[cfg(not(windows))] let cpu=std::env::var("PROCESSOR_IDENTIFIER").ok();
    Inventory{cpu,logical_cpus:std::thread::available_parallelism().ok().map(|n|n.get()),adapters,adapter_scan_complete,adapter_scan_error}
}
/// Conservative job slots from the detected adapter model and NVIDIA's
/// physical NVENC count, not the driver session limit. Unknown models and
/// ambiguous multiple NVIDIA adapters stay serial.
/// https://developer.nvidia.com/video-encode-decode-support-matrix
pub fn queue_parallelism(inv: &Inventory) -> usize {
    if !inv.adapter_scan_complete { return 1; }
    let mut nvidia = inv.adapters.iter().filter(|a| a.instance_id.as_deref()
        .is_some_and(|id| id.to_ascii_uppercase().starts_with("PCI\\VEN_10DE")));
    let Some(adapter) = nvidia.next() else { return 1; };
    if nvidia.next().is_some() { return 1; }
    let Some(name) = adapter.name.as_deref() else { return 1; };
    let name = name.trim().to_ascii_lowercase();
    let name = name.strip_prefix("nvidia ").unwrap_or(&name);
    let name = name.strip_suffix(" laptop gpu").map(|s| format!("{s} laptop"))
        .unwrap_or_else(|| name.to_owned());
    if matches!(name.as_str(),
        "geforce rtx 4070 ti" | "geforce rtx 4070 ti super" |
        "geforce rtx 4080" | "geforce rtx 4080 16gb" | "geforce rtx 4080 super" |
        "geforce rtx 4080 laptop" | "geforce rtx 4090" | "geforce rtx 4090 laptop" |
        "geforce rtx 5070 ti" | "geforce rtx 5080" | "geforce rtx 5080 laptop" |
        "geforce rtx 5090" | "geforce rtx 5090 laptop" |
        "rtx 6000 ada generation" | "l40") { 2 } else { 1 }
}

#[cfg(test)]
#[test]
fn queue_parallelism_requires_known_physical_engines() {
    let mut inv = Inventory { cpu: None, logical_cpus: None, adapters: vec![
        Adapter { name: Some("NVIDIA GeForce RTX 4080".into()), instance_id: Some("PCI\\VEN_10DE&DEV_2704".into()), driver_version: None }
    ], adapter_scan_complete: true, adapter_scan_error: None };
    assert_eq!(queue_parallelism(&inv), 2);
    inv.adapters[0].name = Some("NVIDIA T600".into());
    assert_eq!(queue_parallelism(&inv), 1);
    inv.adapters[0].name = Some("NVIDIA GeForce RTX 5070 Ti Laptop GPU".into());
    assert_eq!(queue_parallelism(&inv), 1);
    inv.adapters[0].name = Some("NVIDIA GeForce RTX 4080 Laptop GPU".into());
    assert_eq!(queue_parallelism(&inv), 2);
    inv.adapters.push(inv.adapters[0].clone());
    assert_eq!(queue_parallelism(&inv), 1);
}

#[derive(Debug,Clone,PartialEq,Eq)]
pub enum Evidence { NotListed, AdapterAbsent, Verified, Failed, TimedOut }
#[derive(Debug,Clone)]
pub struct Capability {pub encoder:String,pub bit_depth:u8,pub evidence:Evidence,pub diagnostic:String}
#[derive(Debug,Clone,PartialEq,Eq)]
pub struct ToolFingerprint {
    pub executable: PathBuf,
    pub size_bytes: u64,
    pub modified: Option<SystemTime>,
}
#[derive(Debug,Clone,PartialEq,Eq)]
struct Fingerprint {tool:ToolFingerprint,version:String,inventory:Inventory}
#[derive(Debug,Clone)]
pub struct Report {pub inventory:Inventory,pub tool_version:String,pub tool:ToolFingerprint,pub capabilities:Vec<Capability>,pub cache_hit:bool}
#[derive(Default)]
pub struct Detector {cache:Option<(Fingerprint,Instant,Report)>}
const ENCODERS:[(&str,u8);18]=[
    ("h264_nvenc",8),("h264_nvenc",10),("hevc_nvenc",8),("hevc_nvenc",10),("av1_nvenc",8),("av1_nvenc",10),
    ("h264_qsv",8),("h264_qsv",10),("hevc_qsv",8),("hevc_qsv",10),("av1_qsv",8),("av1_qsv",10),
    ("h264_amf",8),("h264_amf",10),("hevc_amf",8),("hevc_amf",10),("av1_amf",8),("av1_amf",10),
];
fn checked_text(exe:&Path,args:&[&str],cancel:&AtomicBool)->io::Result<String>{
    let o=process::run(&Request::new(exe).args(args.iter().copied()),cancel)?;
    if o.stop!=Stop::Exited||!o.status.success()||o.stdout.discarded>0{
        return Err(io::Error::other(crate::message!(EncodingToolQuery,format!("{:?}",o.stop),o.status,String::from_utf8_lossy(&o.stderr.bytes))));
    }
    String::from_utf8(o.stdout.bytes).map_err(|e|io::Error::new(io::ErrorKind::InvalidData,e))
}
fn listed(text:&str,encoder:&str)->bool {
    text.lines().any(|line|{
        let mut parts=line.split_whitespace();
        parts.next().is_some_and(|f|f.len()==6&&f.starts_with('V'))&&parts.next()==Some(encoder)
    })
}
fn completed(progress:&str)->bool{
    progress.lines().filter_map(|line|line.strip_prefix("frame=")).filter_map(|x|x.trim().parse::<u64>().ok()).last().is_some_and(|n|n>=3)
    && progress.lines().any(|line|line.trim()=="progress=end")
}
fn try_encoder(exe:&Path,name:&str,bits:u8,cancel:&AtomicBool)->io::Result<Capability>{
    let format=if bits==10 {"p010le"} else {"nv12"};
    let source=format!("color=c=gray:s=256x256:r=30,format={format}");
    let strict=format!("+{format}");
    let mut req=Request::new(exe).args(["-hide_banner","-loglevel","error","-nostdin","-progress","pipe:1","-f","lavfi","-i",&source,"-an","-frames:v","3","-c:v",name,"-pix_fmt",&strict,"-f","null","-"]);
    req.timeout=Duration::from_secs(15);
    let o=process::run(&req,cancel)?;
    if o.stop==Stop::Cancelled||cancel.load(Ordering::Acquire){return Err(io::Error::new(io::ErrorKind::Interrupted,crate::text::en(crate::text::Msg::DetectCancelled)));}
    let evidence=if o.stop==Stop::TimedOut{Evidence::TimedOut}
        else if o.status.success()&&o.stdout.discarded==0&&completed(&String::from_utf8_lossy(&o.stdout.bytes)){Evidence::Verified}
        else{Evidence::Failed};
    let diagnostic=format!("{}; stderr discarded={} bytes; {}",o.status,o.stderr.discarded,String::from_utf8_lossy(&o.stderr.bytes));
    Ok(Capability{encoder:name.into(),bit_depth:bits,evidence,diagnostic})
}
impl Detector{
    /// Discover compiled hardware encoders and present adapters without encoding.
    pub(crate) fn list_for_transcode(&mut self, exe: &Path, cancel: &AtomicBool) -> io::Result<Report> {
        let exe = std::fs::canonicalize(exe)?;
        let meta = std::fs::metadata(&exe)?;
        let inv = inventory();
        let tool = ToolFingerprint {
            executable: exe.clone(),
            size_bytes: meta.len(),
            modified: meta.modified().ok(),
        };
        let listed_encoders = checked_text(&exe, &["-hide_banner", "-encoders"], cancel)?;
        let version = checked_text(&exe, &["-version"], cancel)?;
        let complete_pci_inventory = inv.adapter_scan_complete && inv.adapters.iter().all(|adapter| {
            adapter.instance_id.as_deref().is_some_and(|id| id.starts_with("PCI\\VEN_"))
        });
        let capabilities = ENCODERS.iter().filter_map(|&(encoder, bit_depth)| {
            let vendor = if encoder.ends_with("_nvenc") { "10DE" }
                else if encoder.ends_with("_qsv") { "8086" }
                else { "1002" };
            let present = inv.adapters.iter().any(|adapter| {
                adapter.instance_id.as_deref().and_then(|id| id.strip_prefix("PCI\\VEN_")).and_then(|id| id.get(..4))
                    .is_some_and(|id| id.eq_ignore_ascii_case(vendor))
            });
            let compiled = listed(&listed_encoders, encoder);
            let (evidence, diagnostic) = if !compiled {
                (Evidence::NotListed, crate::text::en(crate::text::Msg::HardwareNotListed))
            } else if complete_pci_inventory && !present {
                (Evidence::AdapterAbsent, crate::text::en(crate::text::Msg::AdapterAbsent))
            } else {
                return None;
            };
            Some(Capability { encoder: encoder.into(), bit_depth, evidence, diagnostic: diagnostic.into() })
        }).collect();
        Ok(Report { inventory: inv, tool_version: version, tool, capabilities, cache_hit: false })
    }

    /// Memory cache only, keyed by present adapters/driver, tool identity and CPU.
    /// Failed/timed-out probes are evidence of this attempt, not permanent unsupported claims.
    pub fn detect(&mut self,exe:&Path,cancel:&AtomicBool)->io::Result<Report>{
        if !exe.is_absolute(){return Err(io::Error::new(io::ErrorKind::InvalidInput,crate::text::en(crate::text::Msg::AbsoluteFfmpeg)));}
        let exe=std::fs::canonicalize(exe)?;
        let meta=std::fs::metadata(&exe)?;
        let version=checked_text(&exe,&["-version"],cancel)?;
        let inv=inventory();
        let key=Fingerprint{tool:ToolFingerprint{executable:exe.clone(),size_bytes:meta.len(),modified:meta.modified().ok()},version:version.clone(),inventory:inv.clone()};
        let cacheable=inv.adapter_scan_complete&&inv.cpu.is_some()&&key.tool.modified.is_some()&&inv.adapters.iter().all(|a|a.instance_id.is_some()&&a.driver_version.is_some());
        if cacheable {
            if let Some((old,at,report))=&self.cache {
                let stable=report.capabilities.iter().all(|c|matches!(c.evidence,Evidence::Verified|Evidence::NotListed));
                if old==&key&&(stable||at.elapsed()<Duration::from_secs(60)) {let mut hit=report.clone();hit.cache_hit=true;return Ok(hit);}
            }
        }
        self.cache=None;
        let list=checked_text(&exe,&["-hide_banner","-encoders"],cancel)?;
        let mut capabilities=Vec::new();
        for (name,bits) in ENCODERS {
            if cancel.load(Ordering::Acquire){return Err(io::Error::new(io::ErrorKind::Interrupted,crate::text::en(crate::text::Msg::DetectCancelled)));}
            capabilities.push(if listed(&list,name){try_encoder(&exe,name,bits,cancel)?}
                else{Capability{encoder:name.into(),bit_depth:bits,evidence:Evidence::NotListed,diagnostic:crate::text::en(crate::text::Msg::HardwareNotListed).into()}});
        }
        let report=Report{inventory:inv,tool_version:version,tool:key.tool.clone(),capabilities,cache_hit:false};
        // A transient timeout or initialization failure must not be cached indefinitely.
        if cacheable{
            self.cache=Some((key,Instant::now(),report.clone()));
        }
        Ok(report)
    }
}
#[cfg(test)]
mod tests{
    use super::*;
    #[test]fn exact_encoder_match(){assert!(listed(" V....D hevc_nvenc NVIDIA","hevc_nvenc"));assert!(!listed(" V....D hevc_nvenc_test","hevc_nvenc"));assert!(!listed(" A....D hevc_nvenc","hevc_nvenc"));}
    #[test]fn verified_requires_actual_frames(){assert!(completed("frame=3\r\nprogress=end\r\n"));assert!(!completed("frame=0\nprogress=end\n"));assert!(!completed("frame=3\nprogress=continue\n"));}
}
