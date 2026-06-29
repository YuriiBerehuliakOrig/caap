//! Link-unit shim: re-exports `caap-sys-runtime` so its `#[no_mangle]` C-ABI
//! surface (`caap_runtime_*` in `ffi.rs`/`ffi_native.rs`) is carried into the
//! staticlib/cdylib artifacts this crate emits. `caap compile --target
//! native_exe` links the `.a` produced here (see
//! `caap-cli/src/commands/native_build.rs`); no symbols are defined locally.

pub use caap_sys_runtime::*;
