fn main() {
    let crate_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let out_dir = std::env::var("OUT_DIR").unwrap();

    // Generate the C header for the FFI layer into OUT_DIR so LLVM-compiled
    // executables and plugins can link against it. OUT_DIR is the only safe
    // target: writing back into the crate source tree breaks read-only builds
    // (e.g. when the crate is built from the registry cache as a dependency).
    cbindgen::Builder::new()
        .with_crate(&crate_dir)
        .with_language(cbindgen::Language::C)
        .with_include_guard("CAAP_SYS_RUNTIME_H")
        .with_sys_include("stddef.h")
        .with_sys_include("stdint.h")
        .with_tab_width(4)
        .generate()
        .expect("cbindgen failed")
        .write_to_file(format!("{out_dir}/caap_sys_runtime.h"));
}
