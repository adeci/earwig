//! Drives the portal backends by hand; used by the GNOME VM test.
//!
//! ```text
//! portal_check serve <fifo>
//! portal_check shortcuts <trigger> <count>
//! ```
//!
//! `serve` opens (or restores) the RemoteDesktop session like the daemon
//! does, then reads one JSON request per line from `<fifo>` (reopened after
//! each writer), e.g. `{"method":"auto","text":"hi"}` with method `auto`,
//! `type`, `paste` or `copy`. `shortcuts` binds the GlobalShortcuts and prints
//! `count` commands. Output is one JSON object per line on stdout.

use std::io::BufRead as _;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use anyhow::bail;
use earwig_core::events::InsertMethod;
use earwig_core::hotkey::HotkeySource as _;
use earwig_core::hotkey::portal::PortalShortcuts;
use earwig_core::insert::Inserter as _;
use earwig_core::insert::portal::PortalInserter;
use serde_json::json;

fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["serve", fifo] => serve(fifo),
        ["shortcuts", trigger, count] => shortcuts(trigger, count.parse()?),
        _ => bail!("usage: see the top of examples/portal_check.rs"),
    }
}

fn serve(fifo: &str) -> anyhow::Result<()> {
    let start = Instant::now();
    let mut inserter = PortalInserter::connect(&earwig_core::paths::state_dir())?;
    emit(&json!({
        "event": "ready",
        "methods": inserter.methods(),
        "connect_ms": start.elapsed().as_millis(),
    }));
    loop {
        for line in std::io::BufReader::new(std::fs::File::open(fifo)?).lines() {
            let request: serde_json::Value = serde_json::from_str(&line?)?;
            let text = request["text"].as_str().unwrap_or_default();
            let start_unix_us = SystemTime::now().duration_since(UNIX_EPOCH)?.as_micros();
            let start = Instant::now();
            let result = match request["method"].as_str() {
                Some("auto") => inserter.insert(text).map(Some),
                Some("type") => inserter
                    .insert_with(InsertMethod::PortalType, text)
                    .map(|()| Some(InsertMethod::PortalType)),
                Some("paste") => inserter
                    .insert_with(InsertMethod::PortalPaste, text)
                    .map(|()| Some(InsertMethod::PortalPaste)),
                Some("copy") => inserter.copy_to_clipboard(text).map(|()| None),
                other => bail!("unknown method {other:?}"),
            };
            let elapsed = start.elapsed();
            emit(&match result {
                Ok(method) => json!({
                    "event": "inserted",
                    "method": method,
                    "chars": text.chars().count(),
                    "start_unix_us": start_unix_us,
                    "insert_us": elapsed.as_micros(),
                }),
                Err(e) => json!({ "event": "insert-failed", "error": e.to_string() }),
            });
        }
    }
}

fn shortcuts(trigger: &str, count: usize) -> anyhow::Result<()> {
    let (tx, rx) = flume::unbounded();
    let source = Box::new(PortalShortcuts::new(trigger));
    std::thread::spawn(move || {
        if let Err(e) = source.run(tx) {
            eprintln!("portal shortcuts failed: {e:#}");
            std::process::exit(1);
        }
    });
    emit(&json!({ "event": "listening" }));
    for command in rx.iter().take(count) {
        emit(&serde_json::to_value(&command)?);
    }
    Ok(())
}

fn emit(value: &serde_json::Value) {
    println!("{value}");
}
