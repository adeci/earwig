//! `earwigw [args]`: runs the `earwig` next to it (`earwig daemon` without
//! arguments) with no console window, which a console program launched from
//! the Start menu or at sign-in would get. Its output is appended to
//! `earwig.log` in earwig's local data folder.
#![cfg_attr(windows, windows_subsystem = "windows")]

use std::ffi::OsString;
use std::fs::{self, File};
use std::path::PathBuf;
use std::process::{Command, ExitCode, Stdio};

/// The log starts over once it grows past this.
const LOG_LIMIT: u64 = 4 << 20;

fn main() -> ExitCode {
    let Ok(exe) = std::env::current_exe() else {
        return ExitCode::FAILURE;
    };
    let earwig = exe.with_file_name(format!("earwig{}", std::env::consts::EXE_SUFFIX));
    let mut args: Vec<OsString> = std::env::args_os().skip(1).collect();
    if args.is_empty() {
        args.push("daemon".into());
    }
    let mut command = Command::new(earwig);
    command.args(args).stdin(Stdio::null());
    if let Some(log) = log() {
        if let Ok(err) = log.try_clone() {
            command.stderr(err);
        }
        command.stdout(log);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        /// `CREATE_NO_WINDOW`: the child gets a console nobody sees.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    match command.status() {
        Ok(status) => status
            .code()
            .and_then(|code| u8::try_from(code).ok())
            .map_or(ExitCode::FAILURE, ExitCode::from),
        Err(_) => ExitCode::FAILURE,
    }
}

/// `earwig.log`, appended to; a log over [`LOG_LIMIT`] is moved aside first
/// (unless a running earwig still writes to it).
fn log() -> Option<File> {
    let dir: PathBuf = dirs::data_local_dir()?.join("earwig");
    fs::create_dir_all(&dir).ok()?;
    let path = dir.join("earwig.log");
    if fs::metadata(&path).is_ok_and(|meta| meta.len() > LOG_LIMIT) {
        let _ = fs::rename(&path, dir.join("earwig.old.log"));
    }
    File::options().create(true).append(true).open(path).ok()
}
