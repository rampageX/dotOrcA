//! Pure Rust application engine. Platform FFI is isolated in process/platform and hardware/windows.
#![deny(unsafe_code)]
pub mod media;
pub mod process;
pub mod hardware;
pub mod budget;
pub mod decision;
pub mod pipeline;
pub mod queue;
pub mod text;
