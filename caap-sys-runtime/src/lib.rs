// caap-sys-runtime: single source of truth for all sys operations.
//
// Every operation's behaviour lives here exactly once. Consumers differ only in
// who owns the per-session handle state (`RuntimeState`) and how arguments are
// marshalled:
//   1. The caap-core interpreter calls `catalog::dispatch` directly (native
//      Rust, zero conversion), holding its own `RuntimeState` per session.
//   2. LLVM-compiled executables and dlopen plugins call the extern "C" exports
//      in ffi.rs, which dispatch against a thread-local `RuntimeState`.

pub mod fs;
pub mod io;
pub mod net;
pub mod os;
pub mod path;
pub mod proc;
pub mod rand;
pub mod time;

pub mod catalog;
pub mod ffi;
pub mod ffi_native;
pub mod ffi_value;

pub use catalog::{
    dispatch, export_catalog, CatalogEntry, PolicyDecision, PolicyRequest, RuntimeState, SysPolicy,
};
pub use ffi_value::{SysError, SysErrorKind};
