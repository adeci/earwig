//! Throwaway probes for earwig's macOS port.
//!
//! ```text
//! earwig-macos-probe perms
//! earwig-macos-probe request microphone|accessibility|input-monitoring
//! earwig-macos-probe pane microphone|accessibility|input-monitoring
//! earwig-macos-probe hotkey [secs]
//! earwig-macos-probe paste <text> [delay secs]
//! earwig-macos-probe mic [secs]
//! earwig-macos-probe transcribe <model.gguf> <clip.wav> metal|cpu|auto
//! earwig-macos-probe overlay [text]          # Right Command drives it
//! earwig-macos-probe overlay-show <secs>     # show it, then quit
//! ```
//!
//! Everything logged also goes to `$PROBE_LOG` when set.

mod ffi;
mod hotkey;
mod mic;
mod overlay;
mod paste;
mod perms;
mod transcribe;

use std::io::Write as _;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub fn log(line: &str) {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() % 100_000_000);
    let line = format!("[{ms:>8}] {line}\n");
    eprint!("{line}");
    if let Some(path) = std::env::var_os("PROBE_LOG")
        && let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
    {
        let _ = file.write_all(line.as_bytes());
    }
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let arg = |i: usize| args.get(i).map(String::as_str);
    let secs = |i: usize, default: u64| arg(i).and_then(|s| s.parse().ok()).unwrap_or(default);
    log(&format!("probe {args:?}, pid {}", std::process::id()));
    match arg(0).unwrap_or("perms") {
        "perms" => perms::report(),
        "request" => perms::request(arg(1).unwrap_or("")),
        "pane" => perms::open_pane(arg(1).unwrap_or("")),
        "hotkey" => hotkey::run(secs(1, 30)),
        "paste" => {
            std::thread::sleep(Duration::from_secs(secs(2, 5)));
            paste::paste(arg(1).unwrap_or("earwig probe paste "));
        }
        "mic" => mic::run(secs(1, 3))?,
        "transcribe" => transcribe::run(
            Path::new(arg(1).unwrap_or("")),
            Path::new(arg(2).unwrap_or("")),
            arg(3).unwrap_or("auto"),
        )?,
        "overlay" => overlay::run(arg(1).unwrap_or("earwig probe").to_owned(), None),
        "overlay-show" => overlay::run(
            "earwig probe".to_owned(),
            Some(Duration::from_secs(secs(1, 10))),
        ),
        other => anyhow::bail!("unknown probe {other}"),
    }
    Ok(())
}
