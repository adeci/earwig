//! On Windows, the app icon goes into the program's resources. On macOS,
//! the app's Info.plist goes into the binary: run outside its app bundle,
//! `earwig` still has its bundle id and the microphone usage text macOS
//! shows when asking for access.

use std::env;
use std::fs;
use std::path::PathBuf;

const PLIST: &str = "../../assets/macos/Info.plist";

fn main() {
    println!("cargo::rerun-if-changed=windows/earwig.rc");
    println!("cargo::rerun-if-changed=../../assets/earwig.ico");
    println!("cargo::rerun-if-changed={PLIST}");
    match env::var("CARGO_CFG_TARGET_OS").as_deref() {
        Ok("windows") => {
            embed_resource::compile(
                "windows/earwig.rc",
                embed_resource::ParamsIncludeDirs(["../../assets"]),
            )
            .manifest_optional()
            .expect("compiling the Windows resources");
        }
        Ok("macos") => embed_info_plist(),
        _ => {}
    }
}

fn embed_info_plist() {
    let version = env::var("CARGO_PKG_VERSION").expect("cargo sets CARGO_PKG_VERSION");
    let plist = fs::read_to_string(PLIST)
        .expect("reading Info.plist")
        .replace("@VERSION@", &version);
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("cargo sets OUT_DIR")).join("Info.plist");
    fs::write(&out, plist).expect("writing Info.plist");
    println!(
        "cargo::rustc-link-arg-bins=-Wl,-sectcreate,__TEXT,__info_plist,{}",
        out.display()
    );
}
