//! transcribe.cpp links `vulkan-1.lib` by name. On Windows that import
//! library comes with the Vulkan SDK, whose installer sets `VULKAN_SDK` but
//! puts nothing on the linker's path.

fn main() {
    println!("cargo::rerun-if-env-changed=VULKAN_SDK");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
        && let Some(sdk) = std::env::var_os("VULKAN_SDK")
    {
        let lib = std::path::Path::new(&sdk).join("Lib");
        println!("cargo::rustc-link-search=native={}", lib.display());
    }
}
