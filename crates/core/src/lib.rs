//! Pure Rust application engine. Platform FFI is isolated in process/platform and hardware/windows.
#![deny(unsafe_code)]
pub mod media;
pub mod process;
pub mod hardware;
pub mod benchmark;
pub mod predict;
pub mod budget;
pub mod decision;
pub mod pipeline;
