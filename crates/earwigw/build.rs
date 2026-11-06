//! On Windows, the app icon goes into the program's resources.

fn main() {
    println!("cargo::rerun-if-changed=../earwig/windows/earwig.rc");
    println!("cargo::rerun-if-changed=../../assets/earwig.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_resource::compile(
            "../earwig/windows/earwig.rc",
            embed_resource::ParamsIncludeDirs(["../../assets"]),
        )
        .manifest_optional()
        .expect("compiling the Windows resources");
    }
}
