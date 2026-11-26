#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(any(windows, test))]
mod bridge;

#[cfg(windows)]
extern "C" {
    fn orca_cui_run() -> i32;
    fn orca_cui_self_test() -> i32;
}

#[cfg(windows)]
fn main() {
    // SAFETY: the CUI entry points run synchronously on this UI thread.
    let result = unsafe {
        if std::env::args_os()
            .nth(1)
            .is_some_and(|arg| arg == "--self-test")
        {
            orca_cui_self_test()
        } else {
            orca_cui_run()
        }
    };
    bridge::shutdown();
    if result != 0 {
        std::process::exit(result);
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("The dotOrcA GUI requires Windows.");
}
