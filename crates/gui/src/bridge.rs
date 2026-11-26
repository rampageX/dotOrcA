//! CUI callbacks run on one thread. Rust copies incoming UTF-8 and keeps
//! one immutable snapshot per frame. No worker calls back into C.
use orca_core::{
    budget::Codec,
    queue::{Job, Phase, Queue, Settings, Target},
};
use std::{cell::RefCell, ffi::c_char, path::Path, ptr, sync::Arc};

#[derive(Default)]
struct Gui {
    queue: Option<Queue>,
    snapshot: Vec<Arc<Job>>,
    notice: String,
}

impl Gui {
    fn queue(&mut self) -> Result<&Queue, String> {
        if self.queue.is_none() {
            let exe = std::env::current_exe().map_err(|e| format!("Cannot locate EXE: {e}"))?;
            let root = exe.parent().ok_or("EXE directory is unavailable.")?;
            self.queue =
                Some(Queue::portable(root.into()).map_err(|e| format!("Cannot start queue: {e}"))?);
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

// SAFETY: the caller supplies a readable buffer of len bytes, held for this
// call. The limit is larger than a UTF-8 Windows extended-length path.
unsafe fn utf8<'a>(value: *const c_char, len: usize, max: usize) -> Result<&'a str, &'static str> {
    if value.is_null() || len == 0 || len > max {
        return Err("Invalid path or capacity field length.");
    }
    let bytes = unsafe { std::slice::from_raw_parts(value.cast::<u8>(), len) };
    std::str::from_utf8(bytes)
        .ok()
        .filter(|value| !value.contains('\0'))
        .ok_or("Invalid UTF-8 in path or capacity field.")
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
            app.queue()?.add(Path::new(path))?;
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
                .map_err(|_| "Enter a whole number for the target.")?;
            let codec = match codec {
                0 => Codec::H264,
                1 => Codec::Hevc,
                2 => Codec::Av1,
                _ => return Err("Invalid output codec.".into()),
            };
            let settings = match mode {
                0 => Settings::new(value, codec, edge)?,
                1 => Settings::percentage(value, codec, edge)?,
                _ => return Err("Invalid capacity mode.".into()),
            };
            app.queue()?.start(settings)?;
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
        let (snapshot, clearing) = app.queue.as_ref().map_or_else(
            || (Vec::new(), false), Queue::snapshot_with_clearing);
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
            .any(|j| j.source_size_pending || (!j.phase.finished() && j.phase != Phase::Waiting));
        let mut text = format!("{finished}/{total} finished   |   {published} saved   |   {skipped} skipped   |   {failed} failed   |   {} cancelled", finished - published - skipped - failed);
        if clearing {
            text.push_str("   |   Stopping and clearing all tasks…");
        }
        if !app.notice.is_empty() {
            text.push_str("   |   ");
            text.push_str(&app.notice);
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

fn source_size(job: &Job) -> String {
    if job.source_size_pending {
        "reading…".into()
    } else {
        job.source_bytes.map_or_else(|| "unknown".into(), readable_size)
    }
}

fn diagnostic_preview(error: &str) -> String {
    // Keep the beginning and end of the captured diagnostic, rather than
    // discarding the actual error after a long FFmpeg prefix.
    if error.len() <= 16 * 1024 {
        return error.to_owned();
    }
    let first = error.floor_char_boundary(4096);
    let last = error.ceil_char_boundary(error.len() - 12 * 1024);
    format!("{}\n[Diagnostic shortened in UI; see task log.]\n{}", &error[..first], &error[last..])
}

#[repr(C)]
pub struct Row {
    id: u32,
    // Bit 0: can cancel.
    flags: u32,
    percent: i32,
    name: [c_char; 1024],
    status: [c_char; 512],
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
            flags: u32::from(!job.phase.finished() && job.phase != Phase::Cancelling),
            percent: job.progress_percent.map_or(-1, i32::from),
            name: [0; 1024],
            status: [0; 512],
        };
        let name = job
            .source
            .file_name()
            .unwrap_or(job.source.as_os_str())
            .to_string_lossy();
        let phase = match job.phase {
            Phase::Waiting => "Queued",
            Phase::Ready => "Waiting for worker",
            Phase::Inspecting => "Inspecting source / hardware",
            Phase::Preflight => "Checking route",
            Phase::Encoding => "Encoding",
            Phase::Validating => "Validating output",
            Phase::Retrying => "Retrying (capacity)",
            Phase::Cancelling => "Cancelling",
            Phase::Published => "Saved",
            Phase::Skipped => "Skipped",
            Phase::Failed => "Failed",
            Phase::Cancelled => "Cancelled",
        };
        let mut status = format!("{phase}  |  Source {}", source_size(job));
        if let Some(settings) = job.settings {
            let target = match settings.target() {
                Target::Bytes(bytes) => format!("{} MiB", bytes / 1_048_576),
                Target::Percent(percent) => {
                    let bytes = job.target_bytes.or_else(|| job.source_bytes.map(|n| settings.limit_bytes(n)));
                    match bytes {
                        Some(n) => format!("{percent}% ({})", readable_size(n)),
                        None => format!("{percent}% (size unknown)"),
                    }
                }
            };
            status.push_str(&format!("  |  Target ≤ {target}"));
            if job.phase != Phase::Skipped {
                status.push_str(&format!("  |  {:?}  |  {} px max", settings.codec(), settings.max_long_edge()));
            } else {
                status.push_str("  |  Original retained");
            }
        } else if job.phase == Phase::Waiting {
            status.push_str("  |  Settings applied on Start");
        }
        if job.retries > 0 {
            status.push_str(&format!("  |  Retries {}", job.retries));
        }
        if let Some(encoder) = job.encoder {
            status.push_str(&format!("  |  {encoder}"));
        }
        if let (Some(bytes), Some(elapsed)) = (job.bytes, job.elapsed) {
            status.push_str(&format!(
                "  |  Output {} in {:.1} s",
                readable_size(bytes),
                elapsed.as_secs_f64()
            ));
        } else if !job.phase.finished() {
            let eta = job.eta.map_or_else(
                || "unknown".into(),
                |eta| {
                    format!(
                        "{:.1}–{:.1} s",
                        eta.total.lower.as_secs_f64(),
                        eta.total.upper.as_secs_f64()
                    )
                },
            );
            let size = job
                .estimated_bytes
                .map_or_else(|| "unknown".into(), |size| format!("{size} bytes"));
            status.push_str(&format!("  |  ETA {eta}  |  Est. size {size}"));
        }
        unsafe {
            write_text(row.name.as_mut_ptr(), row.name.len(), &name);
            write_text(row.status.as_mut_ptr(), row.status.len(), &status);
            out.write(row);
        }
        1
    })
}

#[no_mangle]
pub unsafe extern "C" fn orca_detail(index: u32, out: *mut c_char, capacity: usize) -> i32 {
    boundary(|app| {
        let Some(job) = app.snapshot.get(index as usize) else {
            return 0;
        };
        let mut text = String::new();
        if let Some(error) = &job.encoder_error {
            text.push_str("FFmpeg/process diagnostic:\n");
            text.push_str(&diagnostic_preview(error));
            text.push_str("\n\n");
        }
        text.push_str(&format!("Source size: {}\nSource: {}", source_size(job), job.source.display()));
        if let Some(output) = &job.output {
            text.push_str(&format!("\nOutput: {}", output.display()));
        }
        for line in &job.detail {
            text.push('\n');
            text.push_str(line);
        }
        i32::from(unsafe { write_text(out, capacity, &text) })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_size_and_diagnostic_preserve_units_and_error_tail() {
        assert_eq!(readable_size(0), "0 B");
        assert_eq!(readable_size(1024), "1.00 KiB");
        assert_eq!(readable_size(1048576), "1.00 MiB");
        assert_eq!(readable_size(1 << 40), "1.00 TiB");
        let long = format!("start {}\nencoder failed", "中文🎬".repeat(4000));
        let preview = diagnostic_preview(&long);
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
            assert_eq!(orca_action(1, row.id), 1);
            assert_eq!(orca_detail(0, message.as_mut_ptr(), message.len()), 1);
            assert!(!std::ffi::CStr::from_ptr(message.as_ptr())
                .to_str()
                .unwrap()
                .contains("Cancelled"));
            assert!(orca_frame(message.as_mut_ptr(), message.len(), &mut count) >= 0);
            assert_eq!(orca_detail(0, message.as_mut_ptr(), message.len()), 1);
            assert!(std::ffi::CStr::from_ptr(message.as_ptr())
                .to_str()
                .unwrap()
                .contains("Cancelled"));
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
