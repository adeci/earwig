//! The `[hooks]` commands: external programs that see the final text.
//!
//! - `filter` gets the text on stdin; what it prints replaces the text, minus
//!   trailing line breaks (so `echo`-style output never presses Enter). A
//!   failure, a non-zero exit, output that is not UTF-8, or the timeout keeps
//!   the original text.
//! - `on_insert` gets the inserted text on stdin and is not awaited.
//!
//! Both inherit the daemon's stderr, so their diagnostics land in its log.
//! Each is an argv, run directly: `["sh", "-c", "…"]` for a shell script, or
//! `["cmd", "/C", "…"]` on Windows.
//!
//! Each OS module spawns the filter as a process tree it can kill whole
//! (`spawn` and `Tree::kill`): a process group on Unix, a job object on
//! Windows.

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

#[cfg(unix)]
use unix as os;
#[cfg(windows)]
use windows as os;

use std::io::{Read as _, Write as _};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::config::HooksConfig;

const DEFAULT_FILTER_TIMEOUT: Duration = Duration::from_millis(2000);

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
    let mut child = match command(&config.on_insert, Stdio::null()).spawn() {
        Ok(child) => child,
        Err(e) => {
            log::warn!("on_insert hook {:?} failed: {e}", config.on_insert[0]);
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

fn command(argv: &[String], stdout: Stdio) -> Command {
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..])
        .stdin(Stdio::piped())
        .stdout(stdout)
        .stderr(Stdio::inherit());
    cmd
}

fn run_filter(argv: &[String], text: &str, timeout: Duration) -> anyhow::Result<String> {
    let deadline = Instant::now() + timeout;
    // Its own process tree, so a timeout also kills what a shell filter
    // started.
    let (mut child, tree) = os::spawn(command(argv, Stdio::piped()))?;
    let kill_group = || tree.kill();
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
        kill_group();
        let _ = child.wait();
        anyhow::bail!("timed out after {} ms", timeout.as_millis());
    };
    anyhow::ensure!(status.success(), "exited with {status}");
    let Ok(output) = output else {
        kill_group();
        anyhow::bail!("stdout still open after {} ms", timeout.as_millis());
    };
    let mut out = String::from_utf8(output?)?;
    out.truncate(out.trim_end_matches(['\n', '\r']).len());
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filter_with(argv: Vec<String>, timeout_ms: Option<u64>) -> String {
        let config = HooksConfig {
            filter: argv,
            filter_timeout_ms: timeout_ms,
            on_insert: Vec::new(),
        };
        filter(&config, "hello world")
    }

    #[test]
    fn filter_without_command_is_identity() {
        assert_eq!(filter(&HooksConfig::default(), "same"), "same");
    }

    #[test]
    fn missing_filter_keeps_original() {
        let config = HooksConfig {
            filter: vec!["/nonexistent/earwig-filter".into()],
            ..HooksConfig::default()
        };
        assert_eq!(filter(&config, "hello world"), "hello world");
    }

    /// Wait until `done`, at most 5 s.
    fn eventually(what: &str, mut done: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !done() {
            assert!(Instant::now() < deadline, "{what}");
            thread::sleep(Duration::from_millis(20));
        }
    }

    #[cfg(unix)]
    mod unix {
        use super::*;

        fn sh(script: &str) -> Vec<String> {
            vec!["sh".into(), "-c".into(), script.into()]
        }

        #[test]
        fn filter_replaces_text_and_drops_trailing_newlines() {
            assert_eq!(filter_with(sh("tr a-z A-Z"), None), "HELLO WORLD");
            assert_eq!(filter_with(sh("cat; echo; echo"), None), "hello world");
            assert_eq!(filter_with(sh("cat >/dev/null; printf ''"), None), "");
        }

        #[test]
        fn failing_filter_keeps_original() {
            assert_eq!(filter_with(sh("echo nope; exit 3"), None), "hello world");
            assert_eq!(filter_with(sh("printf '\\377'"), None), "hello world");
        }

        #[test]
        fn slow_filter_times_out_with_original() {
            let started = Instant::now();
            assert_eq!(
                filter_with(sh("sleep 5; echo late"), Some(100)),
                "hello world"
            );
            assert!(started.elapsed() < Duration::from_secs(2));
        }

        /// Killed means gone or a zombie (state `Z`) waiting for its reaper.
        #[cfg(target_os = "linux")]
        fn alive(pid: &str) -> bool {
            std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|s| {
                s.rsplit_once(") ")
                    .is_some_and(|(_, rest)| !rest.starts_with('Z'))
            })
        }

        #[cfg(target_os = "macos")]
        fn alive(pid: &str) -> bool {
            Command::new("ps")
                .args(["-o", "stat=", "-p", pid])
                .output()
                .is_ok_and(|out| {
                    let state = String::from_utf8_lossy(&out.stdout);
                    let state = state.trim();
                    !state.is_empty() && !state.starts_with('Z')
                })
        }

        #[test]
        fn timeout_kills_the_filters_children() {
            let dir = tempfile::tempdir().unwrap();
            let pid_file = dir.path().join("pid");
            let script = format!("sleep 30 & echo $! > '{}'; wait", pid_file.display());
            assert_eq!(filter_with(sh(&script), Some(300)), "hello world");
            let pid = std::fs::read_to_string(&pid_file).unwrap();
            eventually("the filter's child survived", || !alive(pid.trim()));
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
            eventually("hook output never appeared", || {
                std::fs::read_to_string(&out).ok().as_deref() == Some("dictated")
            });
        }
    }

    #[cfg(windows)]
    mod windows {
        use super::*;

        fn cmd(script: &str) -> Vec<String> {
            vec!["cmd".into(), "/C".into(), script.into()]
        }

        #[test]
        fn filter_replaces_text_and_drops_trailing_newlines() {
            assert_eq!(
                filter_with(vec!["findstr".into(), "^".into()], None),
                "hello world"
            );
            assert_eq!(
                filter_with(cmd("echo HELLO WORLD& echo.& echo."), None),
                "HELLO WORLD"
            );
            assert_eq!(filter_with(cmd("more >nul"), None), "");
        }

        #[test]
        fn failing_filter_keeps_original() {
            assert_eq!(filter_with(cmd("echo nope& exit 3"), None), "hello world");
            let dir = tempfile::tempdir().unwrap();
            let latin1 = dir.path().join("latin1");
            std::fs::write(&latin1, b"\xff").unwrap();
            let argv = vec![
                "cmd".into(),
                "/C".into(),
                "type".into(),
                latin1.display().to_string(),
            ];
            assert_eq!(filter_with(argv, None), "hello world");
        }

        #[test]
        fn slow_filter_times_out_with_original() {
            let started = Instant::now();
            let slow = cmd("ping -n 6 127.0.0.1 >nul& echo late");
            assert_eq!(filter_with(slow, Some(100)), "hello world");
            assert!(started.elapsed() < Duration::from_secs(2));
        }

        #[test]
        fn timeout_kills_the_filters_children() {
            let dir = tempfile::tempdir().unwrap();
            // The background ping holds its output file open, which keeps
            // the file from being deleted until it dies.
            let held = dir.path().join("held");
            let script = format!(
                "start /b ping -n 30 127.0.0.1 >{}& ping -n 30 127.0.0.1 >nul",
                held.display()
            );
            assert_eq!(filter_with(cmd(&script), Some(300)), "hello world");
            assert!(held.exists(), "the filter's child never started");
            eventually("the filter's child survived", || {
                std::fs::remove_file(&held).is_ok()
            });
        }

        #[test]
        fn on_insert_receives_text_without_blocking() {
            let dir = tempfile::tempdir().unwrap();
            let out = dir.path().join("inserted");
            let config = HooksConfig {
                on_insert: cmd(&format!(
                    "ping -n 2 127.0.0.1 >nul& findstr ^^ >{}",
                    out.display()
                )),
                ..HooksConfig::default()
            };
            let started = Instant::now();
            on_insert(&config, "dictated");
            assert!(started.elapsed() < Duration::from_millis(150));
            eventually("hook output never appeared", || {
                std::fs::read_to_string(&out).is_ok_and(|text| text.trim_end() == "dictated")
            });
        }
    }
}
