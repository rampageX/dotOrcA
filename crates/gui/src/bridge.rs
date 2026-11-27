//! CUI callbacks run on one thread. Rust copies incoming UTF-8 and keeps
//! one immutable snapshot per frame. No worker calls back into C.
use orca_core::{
    budget::Codec,
    localized, message,
    text::{self, Language, Msg},
    queue::{Job, Paths, Phase, Queue, Settings, Target},
};
use std::{cell::RefCell, ffi::c_char, fs, io, path::{Path, PathBuf}, ptr, sync::Arc};

#[derive(Default)]
struct Gui {
    queue: Option<Queue>,
    snapshot: Vec<Arc<Job>>,
    notice: String,
    language: Language,
    paths: Paths,
}

impl Gui {
    fn queue(&mut self) -> Result<&Queue, String> {
        if self.queue.is_none() {
            let exe = std::env::current_exe().map_err(|e| message!(LocateExe,e))?;
            let root = exe.parent().ok_or(text::en(Msg::ExeDirectory))?;
            self.queue =
                Some(Queue::portable(root.into()).map_err(|e| message!(StartQueue,e))?);
        }
        Ok(self.queue.as_ref().expect("initialized queue"))
    }
}

thread_local! {
    static APP: RefCell<Gui> = RefCell::new(Gui::default());
}

#[cfg(any(windows, test))]
pub fn shutdown() {
    let queue = APP.with_borrow_mut(|app| app.queue.take());
    if let Some(queue) = queue {
        queue.shutdown();
    }
}

fn boundary(action: impl FnOnce(&mut Gui) -> i32) -> i32 {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| APP.with_borrow_mut(action)))
        .unwrap_or(-1)
}


fn config_dir() -> io::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let parent = exe.parent().ok_or_else(|| io::Error::other(text::en(Msg::ExeDirectory)))?;
    Ok(parent.join("config"))
}

mod config;

fn save_config(name: &str, value: &str) -> io::Result<()> {
    config::save(&config_dir()?, name, value)
}
fn read_config(name: &str) -> io::Result<Option<String>> {
    config::read(&config_dir()?, name)
}
#[no_mangle]
pub extern "C" fn orca_ui_init(system_zh: i32) -> i32 {
    boundary(|app| {
        app.language = if system_zh != 0 { Language::Zh } else { Language::En };
        match read_config("ui-language.txt") {
            Ok(Some(s)) => match s.trim() {
                "zh-CN" => app.language = Language::Zh,
                "en" => app.language = Language::En,
                _ => {}
            },
            Err(e) => app.notice = message!(ConfigRead,e),
            Ok(None) => {}
        }
        match read_config("directories.txt") {
            Ok(Some(s)) => if let Some((output,tools)) = s.split_once('\n') {
                if let Ok(paths) = parse_paths(output.trim_end_matches('\r'),tools.trim_end_matches(['\r','\n'])) { app.paths = paths; }
            },
            Err(e) => app.notice = message!(ConfigRead,e),
            Ok(None) => {}
        }
        i32::from(app.language == Language::Zh)
    })
}
fn language() -> Language {
    APP.try_with(|app| app.try_borrow().map_or(Language::En, |app| app.language)).unwrap_or(Language::En)
}
#[no_mangle]
pub extern "C" fn orca_label(id: u32) -> *const c_char {
    text::label(language(), id).as_ptr()
}
#[no_mangle]
pub extern "C" fn orca_set_language(zh: i32) -> i32 {
    boundary(|app| {
        app.language = if zh != 0 { Language::Zh } else { Language::En };
        match save_config("ui-language.txt", if zh != 0 { "zh-CN\n" } else { "en\n" }) {
            Ok(()) => { app.notice.clear(); 1 },
            Err(e) => { app.notice = message!(ConfigError,e); 0 },
        }
    })
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Placement { x:i32, y:i32, width:i32, height:i32, state:i32, has_position:i32 }
impl Placement {
    fn valid(&self) -> bool {
        (360..=8192).contains(&self.width) && (280..=8192).contains(&self.height)
            && (0..=1).contains(&self.state) && (0..=1).contains(&self.has_position)
    }
}
#[no_mangle]
pub unsafe extern "C" fn orca_load_placement(kind: i32, out: *mut Placement) -> i32 {
    if out.is_null() || !out.is_aligned() { return -1; }
    boundary(|app| {
        match read_config(if kind == 0 { "main-placement.txt" } else { "details-placement.txt" }) {
            Ok(Some(s)) => {
                let Ok(values) = s.split_whitespace().map(str::parse::<i32>).collect::<Result<Vec<_>,_>>() else { return 0; };
                let [x,y,width,height,state,has_position] = values.as_slice() else { return 0; };
                let p = Placement{x:*x,y:*y,width:*width,height:*height,state:*state,has_position:*has_position};
                if !p.valid() { return 0; }
                unsafe { out.write(p) };
                1
            }
            Err(e) => { app.notice = message!(ConfigRead,e); 0 }
            Ok(None) => 0,
        }
    })
}
#[no_mangle]
pub unsafe extern "C" fn orca_save_placement(kind: i32, value: *const Placement) -> i32 {
    if value.is_null() || !value.is_aligned() { return -1; }
    boundary(|app| {
        let p = unsafe { value.read() };
        if !p.valid() { return 0; }
        match save_config(if kind == 0 { "main-placement.txt" } else { "details-placement.txt" }, &format!("{} {} {} {} {} {}\n",p.x,p.y,p.width,p.height,p.state,p.has_position)) {
            Ok(()) => 1,
            Err(e) => { app.notice = message!(ConfigError,e); 0 }
        }
    })
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct UiSettings { mib:[c_char;24], percent:[c_char;24], mode:i32, codec:i32, resolution:i32 }
fn settings_valid(mib:&str, percent:&str, mode:i32, codec:i32, resolution:i32) -> bool {
    !mib.is_empty() && !percent.is_empty() && mib.len()<24 && percent.len()<24
        && mib.bytes().all(|b|b.is_ascii_digit()) && percent.bytes().all(|b|b.is_ascii_digit())
        && (0..=1).contains(&mode) && (0..=2).contains(&codec) && (0..=3).contains(&resolution)
}
#[no_mangle]
pub unsafe extern "C" fn orca_load_settings(out:*mut UiSettings) -> i32 {
    if out.is_null() || !out.is_aligned() { return -1; }
    boundary(|app| {
        let s = match read_config("settings.txt") { Ok(Some(s))=>s, Ok(None)=>return 0, Err(e)=>{app.notice=message!(ConfigRead,e);return 0;} };
        let parts:Vec<_>=s.split_whitespace().collect();
        let [mib,pct,mode,codec,res]=parts.as_slice() else {return 0;};
        let (Ok(mode),Ok(codec),Ok(resolution))=(mode.parse::<i32>(),codec.parse::<i32>(),res.parse::<i32>()) else {return 0;};
        if !settings_valid(mib,pct,mode,codec,resolution) { return 0; }
        let mut v=UiSettings{mib:[0;24],percent:[0;24],mode,codec,resolution};
        unsafe { write_text(v.mib.as_mut_ptr(),24,mib);write_text(v.percent.as_mut_ptr(),24,pct);out.write(v); }
        1
    })
}
#[no_mangle]
pub unsafe extern "C" fn orca_save_settings(value:*const UiSettings) -> i32 {
    if value.is_null() || !value.is_aligned() {return -1;}
    boundary(|app| {
        let v=unsafe{value.read()};
        let parse=|buf:&[c_char;24]| -> Option<String> {
            let end=buf.iter().position(|&c|c==0)?;
            String::from_utf8(buf[..end].iter().map(|&c|c as u8).collect()).ok()
        };
        let (Some(mib),Some(pct))=(parse(&v.mib),parse(&v.percent)) else{return 0;};
        if !settings_valid(&mib,&pct,v.mode,v.codec,v.resolution) {return 0;}
        match save_config("settings.txt",&format!("{} {} {} {} {}\n",mib,pct,v.mode,v.codec,v.resolution)) {
            Ok(())=>1,Err(e)=>{app.notice=message!(ConfigError,e);0}
        }
    })
}
fn parse_paths(output:&str,tools:&str) -> Result<Paths,()> {
    let parse=|s:&str| -> Result<Option<PathBuf>,()> {
        if s.is_empty(){return Ok(None);}
        if s.chars().any(char::is_control) || !Path::new(s).is_absolute(){return Err(());}
        Ok(Some(PathBuf::from(s)))
    };
    Ok(Paths{output:parse(output)?,tools:parse(tools)?})
}
#[no_mangle]
pub unsafe extern "C" fn orca_get_directory(kind:i32,out:*mut c_char,cap:usize) -> i32 {
    boundary(|app| {
        let path=if kind==0 {&app.paths.output} else {&app.paths.tools};
        let s=path.as_deref().map(|p|p.to_string_lossy()).unwrap_or_default();
        i32::from(unsafe{write_text(out,cap,&s)})
    })
}
#[no_mangle]
pub unsafe extern "C" fn orca_set_directories(output:*const c_char,on:usize,tools:*const c_char,tn:usize) -> i32 {
    boundary(|app| {
        let out=if on==0 {""} else {match unsafe{utf8(output,on,131072)}{Ok(v)=>v,Err(_)=>return 0}};
        let tool=if tn==0 {""} else {match unsafe{utf8(tools,tn,131072)}{Ok(v)=>v,Err(_)=>return 0}};
        let Ok(paths)=parse_paths(out,tool) else{return 0;};
        if paths.tools.is_some() && paths.resolve_tools(Path::new("")).is_none(){return 0;}
        match save_config("directories.txt",&format!("{out}\n{tool}\n")){
            Ok(())=>{app.paths=paths;app.notice.clear();1},
            Err(e)=>{app.notice=message!(ConfigError,e);0}
        }
    })
}

// SAFETY: the caller supplies a readable buffer of len bytes, held for this
// call. The limit is larger than a UTF-8 Windows extended-length path.
unsafe fn utf8<'a>(value: *const c_char, len: usize, max: usize) -> Result<&'a str, &'static str> {
    if value.is_null() || len == 0 || len > max {
        return Err(text::en(Msg::InvalidLength));
    }
    let bytes = unsafe { std::slice::from_raw_parts(value.cast::<u8>(), len) };
    std::str::from_utf8(bytes)
        .ok()
        .filter(|value| !value.contains('\0'))
        .ok_or(text::en(Msg::InvalidUtf8))
}

// SAFETY: out has capacity writable bytes for the duration of the call.
// Always terminate, truncating on a UTF-8 boundary when space is limited.
unsafe fn write_text(out: *mut c_char, capacity: usize, text: &str) -> bool {
    if out.is_null() || capacity == 0 {
        return false;
    }
    let len = text.floor_char_boundary(text.len().min(capacity - 1));
    unsafe {
        ptr::copy_nonoverlapping(text.as_ptr(), out.cast::<u8>(), len);
        *out.add(len) = 0;
    }
    true
}

#[no_mangle]
pub unsafe extern "C" fn orca_add_path(
    path: *const c_char,
    path_len: usize,
) -> i32 {
    boundary(|app| {
        let added = (|| {
            let path = unsafe { utf8(path, path_len, 131_072) }?;
            let paths = app.paths.clone();
            app.queue()?.add_with_paths(Path::new(path), paths)?;
            Ok::<_, String>(())
        })();
        match added {
            Ok(()) => 1,
            Err(error) => {
                app.notice = error;
                0
            }
        }
    })
}

#[no_mangle]
pub unsafe extern "C" fn orca_start(
    size: *const c_char,
    size_len: usize,
    mode: i32,
    codec: i32,
    edge: u32,
) -> i32 {
    boundary(|app| {
        app.notice.clear();
        let started = (|| {
            let value = unsafe { utf8(size, size_len, 24) }?
                .trim()
                .parse::<u64>()
                .map_err(|_| text::en(Msg::WholeTarget))?;
            let codec = match codec {
                0 => Codec::H264,
                1 => Codec::Hevc,
                2 => Codec::Av1,
                _ => return Err(text::en(Msg::InvalidCodec).into()),
            };
            let settings = match mode {
                0 => Settings::new(value, codec, edge)?,
                1 => Settings::percentage(value, codec, edge)?,
                _ => return Err(text::en(Msg::InvalidMode).into()),
            };
            let paths = app.paths.clone();
            app.queue()?.start_with_paths(settings,paths)?;
            Ok::<_, String>(())
        })();
        match started {
            Ok(()) => 1,
            Err(error) => {
                app.notice = error;
                0
            }
        }
    })
}

#[no_mangle]
pub extern "C" fn orca_action(action: u32, id: u32) -> i32 {
    boundary(|app| {
        app.notice.clear();
        let Some(queue) = &app.queue else {
            return 0;
        };
        match action {
            1 => queue.cancel(id),
            2 => queue.clear_finished(),
            3 => queue.clear_all(),
            4 => if let Err(error) = queue.reset(id) {
                app.notice = error.into();
                return 0;
            },
            _ => return 0,
        }
        1
    })
}

#[no_mangle]
pub unsafe extern "C" fn orca_frame(out: *mut c_char, capacity: usize, count: *mut u32) -> i32 {
    if count.is_null() || !count.is_aligned() || out.is_null() || capacity == 0 {
        return -1;
    }
    boundary(|app| {
        let (snapshot, clearing, slots) = app.queue.as_ref().map_or_else(
            || (Vec::new(), false, 1), Queue::snapshot_status);
        app.snapshot = snapshot;
        let total = app.snapshot.len();
        let finished = app.snapshot.iter().filter(|j| j.phase.finished()).count();
        let published = app
            .snapshot
            .iter()
            .filter(|j| j.phase == Phase::Published)
            .count();
        let skipped = app.snapshot.iter().filter(|j| j.phase == Phase::Skipped).count();
        let failed = app
            .snapshot
            .iter()
            .filter(|j| j.phase == Phase::Failed)
            .count();
         let active = clearing || app
            .snapshot
            .iter()
            .any(|j| j.source_size_pending || j.source_meta_pending || (!j.phase.finished() && j.phase != Phase::Waiting));
        let lang = app.language;
        let mut text = localized!(lang, QueueSummary,finished,total,slots,published,skipped,failed,finished-published-skipped-failed);
        if clearing {
            text.push_str("   |   ");
            text.push_str(text::literal(lang, Msg::Clearing));
        }
        if !app.notice.is_empty() {
            text.push_str("   |   ");
            text.push_str(&text::localize(lang, &app.notice));
        }
        if let Some(error)=app.queue.as_ref().and_then(Queue::log_error) {
            text.push_str("   |   ");
            text.push_str(&text::localize(lang,&error));
        }
        unsafe {
            *count = total as u32;
            write_text(out, capacity, &text);
        }
        i32::from(active)
    })
}

fn readable_size(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = KIB * 1024;
    const GIB: u64 = MIB * 1024;
    const TIB: u64 = GIB * 1024;
    for (unit, label) in [(TIB, "TiB"), (GIB, "GiB"), (MIB, "MiB"), (KIB, "KiB")] {
        if bytes >= unit {
            return format!("{:.2} {label}", bytes as f64 / unit as f64);
        }
    }
    format!("{bytes} B")
}

fn source_size(job: &Job, lang: Language) -> String {
    if job.source_size_pending {
        text::literal(lang,Msg::Reading).into()
    } else {
        job.source_bytes.map_or_else(|| text::literal(lang,Msg::Unknown).into(), readable_size)
    }
}

fn diagnostic_preview(error: &str, lang: Language) -> String {
    // Keep the beginning and end of the captured diagnostic, rather than
    // discarding the actual error after a long FFmpeg prefix.
    if error.len() <= 16 * 1024 {
        return error.to_owned();
    }
    let first = error.floor_char_boundary(4096);
    let last = error.ceil_char_boundary(error.len() - 12 * 1024);
    format!("{}\n{}\n{}", &error[..first], text::literal(lang,Msg::DiagnosticShort), &error[last..])
}

fn phase_text(phase: Phase, lang: Language) -> &'static str { text::literal(lang, match phase {
    Phase::Waiting => Msg::Queued, Phase::Ready => Msg::WaitingWorker, Phase::Inspecting => Msg::Inspecting,
    Phase::Preflight => Msg::Checking, Phase::Encoding => Msg::Encoding, Phase::Validating => Msg::Validating,
    Phase::Retrying => Msg::Retrying, Phase::Cancelling => Msg::Cancelling, Phase::Published => Msg::Saved,
    Phase::Skipped => Msg::Skipped, Phase::Failed => Msg::Failed, Phase::Cancelled => Msg::Cancelled,
}) }
fn target_text(job: &Job, lang: Language) -> Option<String> {
    let settings = job.settings?;
    Some(match settings.target() {
        Target::Bytes(bytes) => format!("{} MiB", bytes / 1_048_576),
        Target::Percent(percent) => {
            let bytes = job.target_bytes.or_else(|| job.source_bytes.map(|n| settings.limit_bytes(n)));
            format!("{percent}% ({})", bytes.map_or_else(|| text::literal(lang,Msg::SizeUnknown).into(),readable_size))
        }
    })
}

#[repr(C)]
pub struct Row {
    id: u32,
    // Bit 0: can cancel; bit 1: terminal row (can reset); bit 2: cancelled.
    flags: u32,
    percent: i32,
    name: [c_char; 1024],
    status: [c_char; 512],
    video: [c_char; 512],
}

#[no_mangle]
pub unsafe extern "C" fn orca_row(index: u32, out: *mut Row, capacity: usize) -> i32 {
    if out.is_null() || capacity != std::mem::size_of::<Row>() || !out.is_aligned() {
        return -1;
    }
    boundary(|app| {
        let Some(job) = app.snapshot.get(index as usize) else {
            return 0;
        };
        let mut row = Row {
            id: job.id,
            flags: u32::from(!job.phase.finished() && job.phase != Phase::Cancelling) | (u32::from(job.phase.finished()) << 1) | (u32::from(job.phase == Phase::Cancelled) << 2),
            percent: job.progress_percent.map_or(-1, i32::from),
            name: [0; 1024],
            status: [0; 512],
            video: [0; 512],
        };
        let name = job
            .source
            .file_name()
            .unwrap_or(job.source.as_os_str())
            .to_string_lossy();
        let lang = app.language;
        let phase = phase_text(job.phase,lang);
        let phase = if let Some(reason) = job.failure.as_deref().or(job.skip_reason) {
            format!("{phase}: {}",text::localize(lang,reason))
        } else { phase.to_owned() };
        let mut status = format!("{phase}  |  {}",localized!(lang,SourceStatus,source_size(job,lang)));
        if let Some(settings) = job.settings {
            let target = target_text(job,lang).unwrap_or_default();
            status.push_str(&format!("  |  {}",localized!(lang,TargetStatus,target)));
            if job.phase != Phase::Skipped {
                status.push_str(&format!("  |  {}  |  {}",settings.codec().label(),localized!(lang,MaxStatus,settings.max_long_edge())));
            } else {
                status.push_str(&format!("  |  {}",text::literal(lang,Msg::OriginalRetained)));
            }
        } else if job.phase == Phase::Waiting {
            status.push_str(&format!("  |  {}",text::literal(lang,Msg::SettingsStart)));
        }
        if job.retries > 0 { status.push_str(&format!("  |  {}",localized!(lang,RetryStatus,job.retries))); }
        if let Some(encoder) = job.encoder { status.push_str(&format!("  |  {encoder}")); }
        if job.phase==Phase::Encoding {if let Some(speed)=job.speed {status.push_str(&format!("  |  {}",localized!(lang,SpeedStatus,format!("{speed:.2}"))));}}
        if let (Some(bytes),Some(elapsed)) = (job.bytes,job.elapsed) {
            status.push_str(&format!("  |  {}",localized!(lang,OutputStatus,readable_size(bytes),format!("{:.1}",elapsed.as_secs_f64()))));
        }
        unsafe {
            write_text(row.name.as_mut_ptr(), row.name.len(), &name);
            write_text(row.status.as_mut_ptr(), row.status.len(), &status);
            let video = if let Some(s) = &job.source_video { text::localize(lang,s) }
                else if job.source_meta_pending { text::literal(lang,Msg::MetadataReading).into() }
                else { String::new() };
            write_text(row.video.as_mut_ptr(), row.video.len(), &video);
            out.write(row);
        }
        1
    })
}

fn detail_section(job: &Job, lang: Language, section: u32) -> String {
    let mut lines = Vec::new();
    match section {
        0 => {
            lines.push(localized!(lang,SourceSize,source_size(job,lang)));
            lines.push(localized!(lang,SourcePath,job.source.display()));
            lines.push(job.source_info.as_deref().map_or_else(|| {
                if job.source_meta_pending {text::literal(lang,Msg::MetadataReading).into()}
                else if let Some(error)=&job.source_meta_error {text::localize(lang,error)}
                else {text::literal(lang,Msg::NoSourceMeta).into()}
            },|s|text::lines(lang,s)));
        }
        1 => {
            if let Some(target) = target_text(job,lang) { lines.push(localized!(lang,TargetStatus,target)); }
            if let Some(settings) = job.settings { lines.push(format!("{} | {}",settings.codec().label(),localized!(lang,MaxStatus,settings.max_long_edge()))); }
            if let Some(info) = &job.target_info { lines.push(localized!(lang,Planned,text::localize(lang,info))); }
            if let Some(path) = &job.destination { lines.push(localized!(lang,Destination,path.display())); }
        }
        2 => {
            if let Some(path) = &job.output { lines.push(localized!(lang,SavedOutput,path.display())); }
            if let (Some(bytes),Some(elapsed)) = (job.bytes,job.elapsed) { lines.push(localized!(lang,OutputStatus,readable_size(bytes),format!("{:.1}",elapsed.as_secs_f64()))); }
            if job.output.is_some() { lines.push(job.output_info.as_deref().map_or_else(||text::literal(lang,Msg::NoOutputMeta).into(),|s|text::lines(lang,s))); }
        }
        3 => {
            lines.push(phase_text(job.phase,lang).into());
            if let Some(reason) = job.skip_reason { lines.push(localized!(lang,SkipDetail,text::localize(lang,reason))); }
            if let Some(encoder) = job.encoder { lines.push(encoder.into()); }
            if let Some(p) = job.progress_percent { lines.push(format!("{p}%")); }
            if job.phase==Phase::Encoding {if let Some(speed)=job.speed {lines.push(localized!(lang,SpeedStatus,format!("{speed:.2}")));}}
            if job.retries > 0 { lines.push(localized!(lang,RetryStatus,job.retries)); }
            lines.extend(job.detail.iter().map(|s|text::localize(lang,s)));
        }
        4 => {
            if let Some(reason) = &job.failure { lines.push(localized!(lang,Failure,text::localize(lang,reason))); }
            if let Some(error) = &job.encoder_error {
                lines.push(text::literal(lang,Msg::ErrorHeader).into());
                // The raw captured FFmpeg diagnostic is not translated.
                lines.push(diagnostic_preview(error,lang));
            }
        }
        _ => {}
    }
    if lines.is_empty() { lines.push(text::literal(lang,Msg::NotAvailable).into()); }
    lines.join("\n")
}

#[no_mangle]
pub unsafe extern "C" fn orca_detail(id: u32, section: u32, out: *mut c_char, capacity: usize) -> i32 {
    boundary(|app| {
        let Some(job) = app.snapshot.iter().find(|job| job.id == id) else { return 0; };
        let lang = app.language;
        let text = if section < 5 { detail_section(job,lang,section) } else if section == 5 {
            (0..5).map(|i|format!("{}\n{}",text::label(lang,24+i).to_string_lossy(),detail_section(job,lang,i))).collect::<Vec<_>>().join("\n\n")
        } else { return -1; };
        i32::from(unsafe { write_text(out,capacity,&text) })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_values_and_paths_are_validated() {
        assert!(settings_valid("100","50",1,2,3));
        assert!(!settings_valid("100","50",1,2,4));
        assert!(!settings_valid("100","bad",0,1,0));
        assert!(parse_paths("","").is_ok());
        assert!(parse_paths("relative","").is_err());
        let abs=std::env::temp_dir();
        assert_eq!(parse_paths(abs.to_str().unwrap(),"").unwrap().output,Some(abs));
    }

    #[test]
    fn human_size_and_diagnostic_preserve_units_and_error_tail() {
        assert_eq!(readable_size(0), "0 B");
        assert_eq!(readable_size(1024), "1.00 KiB");
        assert_eq!(readable_size(1048576), "1.00 MiB");
        assert_eq!(readable_size(1 << 40), "1.00 TiB");
        let long = format!("start {}\nencoder failed", "中文🎬".repeat(4000));
        let preview = diagnostic_preview(&long,Language::En);
        assert!(preview.starts_with("start"));
        assert!(preview.ends_with("encoder failed"));
        assert!(preview.len() < 17000);
    }

    #[test]
    fn boundary_copies_paths_validates_sizes_and_keeps_frame_consistent() {
        let base = std::env::temp_dir();
        let name = "小K 的鹅腿坟场 & (黑洞) ^ % ! 🎬.avi";
        let path = base.join(name);
        let mut input = path.to_str().unwrap().as_bytes().to_vec();
        let size = b"30";
        let mut count = 0;
        let mut message = [0; 512];
        unsafe {
            assert_eq!(
                orca_add_path(input.as_ptr().cast(), input.len()),
                1
            );
            input.fill(0);
            assert_eq!(
                orca_add_path(input.as_ptr().cast(), input.len()),
                0
            );
            assert!(orca_frame(message.as_mut_ptr(), message.len(), &mut count) >= 0);
            assert_eq!(count, 1);
            assert_eq!(orca_start(size.as_ptr().cast(), 0, 0, 1, 1920), 0);
            assert_eq!(orca_start(size.as_ptr().cast(), size.len(), 0, -1, 1920), 0);
            assert_eq!(orca_start(b"101".as_ptr().cast(), 3, 1, 1, 1920), 0);
            assert_eq!(orca_start(size.as_ptr().cast(), size.len(), 2, 1, 1920), 0);
            let mut row = std::mem::MaybeUninit::<Row>::uninit();
            assert_eq!(orca_row(0, row.as_mut_ptr(), 1), -1);
            assert_eq!(orca_row(0, row.as_mut_ptr(), std::mem::size_of::<Row>()), 1);
            let row = row.assume_init();
            assert_eq!(
                std::ffi::CStr::from_ptr(row.name.as_ptr())
                    .to_str()
                    .unwrap(),
                name
            );
            assert!(std::ffi::CStr::from_ptr(row.status.as_ptr()).to_str().unwrap()
                .contains("Settings applied on Start"));
            APP.with_borrow_mut(|app| app.language = Language::Zh);
            assert_eq!(orca_detail(row.id, 0, message.as_mut_ptr(), message.len()), 1);
            assert!(std::ffi::CStr::from_ptr(message.as_ptr()).to_str().unwrap().contains("源文件"));
            APP.with_borrow_mut(|app| app.language = Language::En);
            assert_eq!(orca_action(1, row.id), 1);
            assert_eq!(orca_detail(row.id, 3, message.as_mut_ptr(), message.len()), 1);
            assert!(!std::ffi::CStr::from_ptr(message.as_ptr())
                .to_str()
                .unwrap()
                .contains("Cancelled"));
            assert!(orca_frame(message.as_mut_ptr(), message.len(), &mut count) >= 0);
            assert_eq!(orca_detail(row.id, 3, message.as_mut_ptr(), message.len()), 1);
            assert!(std::ffi::CStr::from_ptr(message.as_ptr())
                .to_str()
                .unwrap()
                .contains("Cancelled"));
            let mut reset_row = std::mem::MaybeUninit::<Row>::uninit();
            assert_eq!(orca_row(0, reset_row.as_mut_ptr(), std::mem::size_of::<Row>()), 1);
            let reset_row = reset_row.assume_init();
            assert_eq!(reset_row.flags, 6);
            assert_eq!(orca_action(4, reset_row.id), 1);
            assert!(orca_frame(message.as_mut_ptr(), message.len(), &mut count) >= 0);
            assert_eq!(count, 1);
            let mut fresh_row = std::mem::MaybeUninit::<Row>::uninit();
            assert_eq!(orca_row(0, fresh_row.as_mut_ptr(), std::mem::size_of::<Row>()), 1);
            let fresh_row = fresh_row.assume_init();
            assert_eq!(fresh_row.flags, 1);
            assert_ne!(fresh_row.id, reset_row.id);
            assert_eq!(orca_detail(reset_row.id, 0, message.as_mut_ptr(), message.len()), 0);
            assert_eq!(orca_detail(fresh_row.id, 0, message.as_mut_ptr(), message.len()), 1);
            assert_eq!(orca_action(4, fresh_row.id), 0);
            assert_eq!(orca_action(3, 0), 1);
            assert!(orca_frame(message.as_mut_ptr(), message.len(), &mut count) >= 0);
            assert_eq!(count, 0);
            let mut small = [123; 3];
            assert!(write_text(small.as_mut_ptr(), small.len(), "🎬"));
            assert_eq!(small, [0, 123, 123]);
            assert_eq!(orca_frame(ptr::null_mut(), 0, &mut count), -1);
        }
        shutdown();
    }
}
