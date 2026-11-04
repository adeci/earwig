//! Drives the portal backends by hand; used by the GNOME VM test.
//!
//! ```text
//! portal_check insert <auto|type|paste> <text|@file> [linger-secs]
//! portal_check copy <text|@file> [linger-secs]
//! portal_check shortcuts <trigger> <count>
//! ```
//!
//! `insert`/`copy` open (or restore) the RemoteDesktop session, act once and
//! keep the session alive for `linger-secs` (default 3) so a paste can still
//! fetch the clipboard. `shortcuts` binds the GlobalShortcuts and prints
//! `count` commands. Output is one JSON object per line on stdout.

use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
use earwig_core::events::InsertMethod;
use earwig_core::hotkey::HotkeySource as _;
use earwig_core::hotkey::portal::PortalShortcuts;
use earwig_core::insert::Inserter as _;
use earwig_core::insert::portal::PortalInserter;

fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["insert", method, text, rest @ ..] => {
            let method = match *method {
                "auto" => None,
                "type" => Some(InsertMethod::PortalType),
                "paste" => Some(InsertMethod::PortalPaste),
                other => bail!("unknown method {other}"),
            };
            let text = load(text)?;
            let mut inserter = connect()?;
            let start = Instant::now();
            let used = match method {
                Some(method) => inserter.insert_with(method, &text).map(|()| method),
                None => inserter.insert(&text),
            };
            let elapsed = start.elapsed();
            match used {
                Ok(method) => println!(
                    "{{\"event\":\"inserted\",\"method\":{},\"chars\":{},\"insert_us\":{}}}",
                    serde_json::to_string(&method)?,
                    text.chars().count(),
                    elapsed.as_micros()
                ),
                Err(e) => println!(
                    "{{\"event\":\"insert-failed\",\"error\":{}}}",
                    serde_json::to_string(&e.to_string())?
                ),
            }
            linger(rest)?;
        }
        ["copy", text, rest @ ..] => {
            let text = load(text)?;
            let mut inserter = connect()?;
            inserter.copy_to_clipboard(&text)?;
            println!("{{\"event\":\"copied\"}}");
            linger(rest)?;
        }
        ["shortcuts", trigger, count] => {
            let count: usize = count.parse()?;
            let (tx, rx) = flume::unbounded();
            let source = Box::new(PortalShortcuts::new(trigger));
            std::thread::spawn(move || {
                if let Err(e) = source.run(tx) {
                    eprintln!("portal shortcuts failed: {e:#}");
                    std::process::exit(1);
                }
            });
            println!("{{\"event\":\"listening\"}}");
            for command in rx.iter().take(count) {
                println!("{}", serde_json::to_string(&command)?);
            }
        }
        _ => bail!("usage: see the top of examples/portal_check.rs"),
    }
    Ok(())
}

fn connect() -> anyhow::Result<PortalInserter> {
    let start = Instant::now();
    let inserter = PortalInserter::connect(&earwig_core::paths::state_dir())?;
    println!(
        "{{\"event\":\"ready\",\"methods\":{},\"connect_ms\":{}}}",
        serde_json::to_string(&inserter.methods())?,
        start.elapsed().as_millis()
    );
    Ok(inserter)
}

fn load(text: &str) -> anyhow::Result<String> {
    match text.strip_prefix('@') {
        Some(path) => std::fs::read_to_string(path).with_context(|| format!("reading {path}")),
        None => Ok(text.to_owned()),
    }
}

fn linger(rest: &[&str]) -> anyhow::Result<()> {
    let secs = rest.first().map_or(Ok(3), |s| s.parse())?;
    std::thread::sleep(Duration::from_secs(secs));
    Ok(())
}
