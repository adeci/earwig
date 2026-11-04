//! The `[hooks]` commands: external programs that see the final text.
//!
//! - `filter` gets the text on stdin; what it prints replaces the text.
//!   Trailing line breaks are dropped (so `echo`-style output never presses
//!   Enter). A failure, a non-zero exit, output that is not UTF-8, or the
//!   timeout keeps the original text.
//! - `on_insert` gets the inserted text on stdin and is not awaited.
//!
//! Both inherit the daemon's stderr, so their diagnostics land in its log.

use std::io::{Read as _, Write as _};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::config::HooksConfig;

/// Filter timeout when the config does not set one.
pub const DEFAULT_FILTER_TIMEOUT: Duration = Duration::from_millis(2000);

/// Run the filter over `text`; returns `text` unchanged when no filter is set
/// or it fails.
pub fn filter(config: &HooksConfig, text: &str) -> String {
    if config.filter.is_empty() {
        return text.to_owned();
    }
    let timeout = config
        .filter_timeout_ms
        .map_or(DEFAULT_FILTER_TIMEOUT, Duration::from_millis);
    match run_filter(&config.filter, text, timeout) {
        Ok(out) => out,
        Err(e) => {
            log::warn!(
                "filter hook {:?} failed, inserting unfiltered text: {e:#}",
                config.filter[0]
            );
            text.to_owned()
        }
    }
}

/// Start the `on_insert` command with `text` on stdin and return at once.
pub fn on_insert(config: &HooksConfig, text: &str) {
    if config.on_insert.is_empty() {
        return;
    }
    let mut child = match spawn(&config.on_insert, Stdio::null()) {
        Ok(child) => child,
        Err(e) => {
            log::warn!("on_insert hook {:?} failed: {e:#}", config.on_insert[0]);
            return;
        }
    };
    let name = config.on_insert[0].clone();
    let text = text.to_owned();
    // Feeds stdin and reaps the child so it never lingers as a zombie.
    let reaper = thread::Builder::new()
        .name("earwig-on-insert".into())
        .spawn(move || {
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(text.as_bytes());
            }
            match child.wait() {
                Ok(status) if !status.success() => {
                    log::warn!("on_insert hook {name:?} exited with {status}");
                }
                Ok(_) => {}
                Err(e) => log::warn!("on_insert hook {name:?}: {e}"),
            }
        });
    if let Err(e) = reaper {
        log::warn!("on_insert hook: {e}");
    }
}

fn spawn(argv: &[String], stdout: Stdio) -> anyhow::Result<Child> {
    let child = Command::new(&argv[0])
        .args(&argv[1..])
        .stdin(Stdio::piped())
        .stdout(stdout)
        .stderr(Stdio::inherit())
        .spawn()?;
    Ok(child)
}

fn run_filter(argv: &[String], text: &str, timeout: Duration) -> anyhow::Result<String> {
    let deadline = Instant::now() + timeout;
    let mut child = spawn(argv, Stdio::piped())?;
    let mut stdin = child.stdin.take().expect("stdin is piped");
    let mut stdout = child.stdout.take().expect("stdout is piped");
    let input = text.to_owned();
    // Writer and reader run apart from the wait so a filter that reads
    // slowly, writes a lot, or hangs cannot block past the deadline.
    thread::spawn(move || {
        let _ = stdin.write_all(input.as_bytes());
    });
    let (tx, rx) = flume::bounded(1);
    thread::spawn(move || {
        let mut out = Vec::new();
        let _ = tx.send(stdout.read_to_end(&mut out).map(|_| out));
    });
    let output = rx.recv_deadline(deadline);
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break Some(status);
        }
        if Instant::now() >= deadline {
            break None;
        }
        thread::sleep(Duration::from_millis(5));
    };
    let Some(status) = status else {
        let _ = child.kill();
        let _ = child.wait();
        anyhow::bail!("timed out after {} ms", timeout.as_millis());
    };
    anyhow::ensure!(status.success(), "exited with {status}");
    let Ok(output) = output else {
        anyhow::bail!("stdout still open after {} ms", timeout.as_millis());
    };
    let mut out = String::from_utf8(output?)?;
    out.truncate(out.trim_end_matches(['\n', '\r']).len());
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(script: &str) -> Vec<String> {
        vec!["sh".into(), "-c".into(), script.into()]
    }

    fn filter_with(script: &str, timeout_ms: Option<u64>) -> String {
        let config = HooksConfig {
            filter: sh(script),
            filter_timeout_ms: timeout_ms,
            on_insert: Vec::new(),
        };
        filter(&config, "hello world")
    }

    #[test]
    fn filter_replaces_text_and_drops_trailing_newlines() {
        assert_eq!(filter_with("tr a-z A-Z", None), "HELLO WORLD");
        assert_eq!(filter_with("cat; echo; echo", None), "hello world");
        assert_eq!(filter_with("cat >/dev/null; printf ''", None), "");
    }

    #[test]
    fn failing_filter_keeps_original() {
        assert_eq!(filter_with("echo nope; exit 3", None), "hello world");
        assert_eq!(filter_with("printf '\\377'", None), "hello world");
        let config = HooksConfig {
            filter: vec!["/nonexistent/earwig-filter".into()],
            ..HooksConfig::default()
        };
        assert_eq!(filter(&config, "hello world"), "hello world");
    }

    #[test]
    fn slow_filter_times_out_with_original() {
        let started = Instant::now();
        assert_eq!(filter_with("sleep 5; echo late", Some(100)), "hello world");
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn filter_without_command_is_identity() {
        assert_eq!(filter(&HooksConfig::default(), "same"), "same");
    }

    #[test]
    fn on_insert_receives_text_without_blocking() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("inserted");
        let config = HooksConfig {
            on_insert: sh(&format!("sleep 0.2; cat > '{}'", out.display())),
            ..HooksConfig::default()
        };
        let started = Instant::now();
        on_insert(&config, "dictated");
        assert!(started.elapsed() < Duration::from_millis(150));
        let deadline = Instant::now() + Duration::from_secs(5);
        while std::fs::read_to_string(&out).ok().as_deref() != Some("dictated") {
            assert!(Instant::now() < deadline, "hook output never appeared");
            thread::sleep(Duration::from_millis(20));
        }
    }
}
