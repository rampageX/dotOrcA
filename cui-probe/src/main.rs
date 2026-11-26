#![windows_subsystem = "windows"]
use arena_common::Worker;
use std::{ffi::c_char, sync::OnceLock};

static WORKER: OnceLock<Worker> = OnceLock::new();

#[no_mangle]
pub extern "C" fn arena_worker_start() {
    WORKER.get_or_init(Worker::default).start();
}

// Returns 1 while an update is needed. A single textual snapshot determines
// wakeup, so completion between separate loads cannot leave a stale status.
#[no_mangle]
pub unsafe extern "C" fn arena_worker_status(out: *mut c_char, capacity: usize) -> i32 {
    let text = WORKER.get_or_init(Worker::default).text();
    if out.is_null() || capacity <= text.len() { return 0; }
    std::ptr::copy_nonoverlapping(text.as_ptr(), out.cast::<u8>(), text.len());
    *out.add(text.len()) = 0;
    i32::from(!text.contains("idle") && !text.contains("DONE"))
}

extern "C" { fn arena_cui_run() -> i32; }
fn main() {
    let code = unsafe { arena_cui_run() };
    if code != 0 { std::process::exit(code); }
}
