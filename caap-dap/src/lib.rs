//! CAAP compile-time (CTFE) debugger — library surface.
//!
//! `main.rs` wires these modules to a stdio Debug Adapter Protocol loop. The
//! controller and worker are exercised directly by unit/integration tests.

pub mod controller;
pub mod dap_types;
pub mod protocol;
pub mod stdio_capture;
pub mod wire;
pub mod worker;
