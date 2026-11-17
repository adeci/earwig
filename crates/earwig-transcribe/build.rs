//! transcribe.cpp links `vulkan-1.lib` by name. On Windows that import
//! library comes with the Vulkan SDK, whose installer sets `VULKAN_SDK` but
//! puts nothing on the linker's path.
//!
//! `pinned_avx2`: the Windows packages build transcribe.cpp for AVX2 CPUs
//! (packaging/windows/release-env.ps1), so the engine checks the CPU before it
//! loads rather than dying on an illegal instruction.

fn main() {
    println!("cargo::rerun-if-env-changed=VULKAN_SDK");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
        && let Some(sdk) = std::env::var_os("VULKAN_SDK")
    {
        let lib = std::path::Path::new(&sdk).join("Lib");
        println!("cargo::rustc-link-search=native={}", lib.display());
    }
    println!("cargo::rerun-if-env-changed=TRANSCRIBE_CMAKE_ARGS");
    println!("cargo::rustc-check-cfg=cfg(pinned_avx2)");
    if std::env::var("TRANSCRIBE_CMAKE_ARGS").is_ok_and(|args| args.contains("-DGGML_AVX2=ON")) {
        println!("cargo::rustc-cfg=pinned_avx2");
    }
}
