//! On macOS, embed the app's Info.plist in the binary: run outside its app
//! bundle, `earwig` still has its bundle id and the microphone usage text
//! macOS shows when asking for access.

use std::env;
use std::fs;
use std::path::PathBuf;

const PLIST: &str = "../../assets/macos/Info.plist";

fn main() {
    println!("cargo::rerun-if-changed={PLIST}");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
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
