//! Standalone S03 acceptance CLI; no GUI or decision policy.
#![deny(unsafe_code)]
use orca_core::{hardware,predict::{Database,Query,tool_sha256}};
use serde_json::{Value,json};
use std::{collections::BTreeSet,io,path::PathBuf,sync::atomic::AtomicBool};

fn run()->io::Result<()>{
    let exe=std::env::current_exe()?;
    let root=exe.parent().ok_or_else(||io::Error::other("EXE has no parent"))?;
    let suffix=std::env::consts::EXE_SUFFIX;
    let mut ffmpeg=root.join("tools").join(format!("ffmpeg{suffix}"));
    let mut ffprobe=root.join("tools").join(format!("ffprobe{suffix}"));
    let mut local=None;
    let mut mode=None;
    let mut input=None;
    let mut args=std::env::args_os().skip(1);
    while let Some(a)=args.next(){
        match a.to_str(){
            Some("--help"|"-h")=>{println!("orca-predict --info | --holdout | --probe | --validate PATH [--ffmpeg PATH --ffprobe PATH] [--local DATASET]\n--info: validate versioned bundled data; --holdout: CPU cross-validation; --probe: actual inventory/capability and tool fingerprints, reference estimates only.");return Ok(())}
            Some("--info"|"--holdout"|"--probe") if mode.is_none()=>mode=a.into_string().ok(),
            Some("--validate") if mode.is_none()=>{mode=Some("--validate".into());input=Some(PathBuf::from(args.next().ok_or_else(||io::Error::other("missing source path"))?));},
            Some("--ffmpeg")=>ffmpeg=PathBuf::from(args.next().ok_or_else(||io::Error::other("missing ffmpeg path"))?),
            Some("--ffprobe")=>ffprobe=PathBuf::from(args.next().ok_or_else(||io::Error::other("missing ffprobe path"))?),
            Some("--local") if local.is_none()=>local=Some(PathBuf::from(args.next().ok_or_else(||io::Error::other("missing local dataset path"))?)),
            _=>return Err(io::Error::other("unknown or duplicate argument"))
        }
    }
    let mut db=Database::bundled()?;
    if let Some(p)=local{db=db.with_local(&p)?;}
    if mode.as_deref()==Some("--info"){println!("{}",serde_json::to_string_pretty(&db.info()).map_err(io::Error::other)?);return Ok(())}
    if mode.as_deref()==Some("--holdout"){println!("{}",serde_json::to_string_pretty(&db.cpu_holdout()).map_err(io::Error::other)?);return Ok(())}
    if !matches!(mode.as_deref(),Some("--probe"|"--validate")){return Err(io::Error::other("select --info, --holdout, --probe or --validate"))}
    let ffmpeg=std::fs::canonicalize(ffmpeg)?;let ffprobe=std::fs::canonicalize(ffprobe)?;
    let cancel=AtomicBool::new(false);
    let report=hardware::Detector::default().detect(&ffmpeg,&cancel)?;
    let ffhash=tool_sha256(&ffmpeg,&ffmpeg,&cancel)?;
    let fphash=tool_sha256(&ffmpeg,&ffprobe,&cancel)?;
    if let Some(input)=input{
        let mut v=orca_core::predict::sample::validate(&ffmpeg,&ffprobe,&input,&report,&cancel)?;
        v["ffmpeg_sha256"]=json!(ffhash);v["ffprobe_sha256"]=json!(fphash);
        v["cpu"]=json!(report.inventory.cpu);v["hardware"]=db.hardware_mapping(&report.inventory);
        v["adapters"]=json!(report.inventory.adapters.iter().map(|a|json!({"name":a.name,"driver_version":a.driver_version})).collect::<Vec<_>>());
        println!("{}",serde_json::to_string_pretty(&v).map_err(io::Error::other)?);return Ok(())
    }
    let mut seen=BTreeSet::new();let mut results:Vec<Value>=Vec::new();
    let mut measured=0;let mut inferred=0;let mut unknown=0;
    for (_,f,r) in db.reference_cases(){
        let id=r["case_id"].as_str().ok_or_else(||io::Error::other("case id"))?;
        if !seen.insert(id){continue}
        let q=Query{inventory:&report.inventory,capabilities:&report.capabilities,ffmpeg_sha256:&ffhash,ffprobe_sha256:&fphash,encoder:r["encoder"].as_str().unwrap(),preset:r["preset"].as_str().unwrap(),width:f["width"].as_u64().unwrap() as u32,height:f["height"].as_u64().unwrap() as u32,bit_depth:r["bit_depth"].as_u64().unwrap() as u8,target_bitrate_bps:f["target_bitrate_bps"].as_u64().unwrap(),fixture_decoded_sha256:f["decoded_sha256"].as_str(),frames:180};
        let x=db.estimate(&q);
        match x["kind"].as_str(){Some("measured_reference")=>measured+=1,Some("inferred_cpu_reference")=>inferred+=1,_=>unknown+=1}
        results.push(json!({"case_id":id,"estimate":x}));
    }
    println!("{}",serde_json::to_string_pretty(&json!({"database":db.info(),"hardware":db.hardware_mapping(&report.inventory),"cpu":report.inventory.cpu,"logical_cpus":report.inventory.logical_cpus,"adapter_scan_complete":report.inventory.adapter_scan_complete,"ffmpeg_sha256":ffhash,"ffprobe_sha256":fphash,"capabilities":report.capabilities.iter().map(|c|json!({"encoder":c.encoder,"bit_depth":c.bit_depth,"evidence":format!("{:?}",c.evidence)})).collect::<Vec<_>>(),"measured":measured,"inferred":inferred,"unknown":unknown,"results":results})).map_err(io::Error::other)?);
    Ok(())
}
fn main(){if let Err(e)=run(){eprintln!("S03: {e}");std::process::exit(1)}}
