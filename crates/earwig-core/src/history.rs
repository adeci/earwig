//! Recent transcripts, kept locally so a failed insertion is never lost.
//!
//! One JSON object per line, newest last, trimmed to the configured limit on
//! every append.

use std::fs;
use std::io::{self, BufRead as _, BufReader};
use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::events::InsertMethod;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryEntry {
    /// Unix time in milliseconds when the recording ended.
    pub at_ms: u64,
    pub text: String,
    pub duration_ms: u64,
    /// Model spec string that produced it.
    pub model: String,
    /// `None` when insertion failed.
    pub method: Option<InsertMethod>,
}

#[derive(Debug)]
pub struct History {
    path: PathBuf,
    state: Mutex<State>,
}

#[derive(Debug)]
struct State {
    limit: usize,
    entries: Vec<HistoryEntry>,
}

impl History {
    /// Open (or create on first append) the history at `path`, keeping at
    /// most `limit` entries. Unreadable lines are skipped.
    pub fn open(path: PathBuf, limit: usize) -> io::Result<Self> {
        let entries = match fs::File::open(&path) {
            Ok(file) => BufReader::new(file)
                .lines()
                .map_while(Result::ok)
                .filter_map(|line| serde_json::from_str(&line).ok())
                .collect(),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e),
        };
        let history = Self {
            path,
            state: Mutex::new(State { limit, entries }),
        };
        history.trim()?;
        Ok(history)
    }

    /// Newest first.
    pub fn entries(&self) -> Vec<HistoryEntry> {
        let state = self.lock();
        state.entries.iter().rev().cloned().collect()
    }

    pub fn append(&self, entry: HistoryEntry) -> io::Result<()> {
        self.lock().entries.push(entry);
        self.trim()
    }

    pub fn set_limit(&self, limit: usize) -> io::Result<()> {
        self.lock().limit = limit;
        self.trim()
    }

    pub fn clear(&self) -> io::Result<()> {
        self.lock().entries.clear();
        self.save()
    }

    fn trim(&self) -> io::Result<()> {
        {
            let mut state = self.lock();
            let excess = state.entries.len().saturating_sub(state.limit);
            state.entries.drain(..excess);
        }
        self.save()
    }

    fn save(&self) -> io::Result<()> {
        let state = self.lock();
        if state.entries.is_empty() && !self.path.exists() {
            return Ok(());
        }
        let mut out = String::new();
        for entry in &state.entries {
            out.push_str(&serde_json::to_string(entry).map_err(io::Error::other)?);
            out.push('\n');
        }
        if let Some(dir) = self.path.parent() {
            fs::create_dir_all(dir)?;
        }
        let tmp = self.path.with_extension("jsonl.tmp");
        fs::write(&tmp, out)?;
        fs::rename(&tmp, &self.path)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(text: &str) -> HistoryEntry {
        HistoryEntry {
            at_ms: 0,
            text: text.into(),
            duration_ms: 0,
            model: "m".into(),
            method: Some(InsertMethod::InputMethod),
        }
    }

    #[test]
    fn keeps_newest_within_limit_across_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("h.jsonl");
        let history = History::open(path.clone(), 2).unwrap();
        for text in ["a", "b", "c"] {
            history.append(entry(text)).unwrap();
        }
        let texts = |h: &History| h.entries().into_iter().map(|e| e.text).collect::<Vec<_>>();
        assert_eq!(texts(&history), ["c", "b"]);
        assert_eq!(texts(&History::open(path, 2).unwrap()), ["c", "b"]);
    }
}
