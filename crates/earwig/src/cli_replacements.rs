//! `earwig replacements ...`: list, add and remove what earwig writes instead
//! of what it heard.

use clap::Subcommand;
use earwig_core::config::ConfigStore;
use earwig_core::events::Command;
use earwig_core::replacements::normalize;

#[derive(Subcommand)]
pub enum ReplacementsCmd {
    /// What earwig heard, then what it writes instead; `*` marks the ones
    /// the system config sets.
    List,
    /// Write something else wherever earwig hears a phrase.
    Add {
        /// The words as earwig hears them, in any capitals.
        heard: String,
        /// What it writes instead, exactly as given.
        written: String,
    },
    /// Stop replacing a phrase.
    Remove {
        /// The words as earwig hears them, in any capitals.
        heard: String,
    },
}

pub fn run(cmd: ReplacementsCmd) -> anyhow::Result<()> {
    match cmd {
        ReplacementsCmd::List => list(),
        ReplacementsCmd::Add { heard, written } => add(&heard, &written),
        ReplacementsCmd::Remove { heard } => remove(&heard),
    }
}

fn list() -> anyhow::Result<()> {
    let store = ConfigStore::load()?;
    let table = &store.config().replacements;
    if table.is_empty() {
        println!("no replacements; add one with `earwig replacements add <HEARD> <WRITTEN>`");
        return Ok(());
    }
    let width = table
        .keys()
        .map(|heard| heard.chars().count())
        .max()
        .unwrap_or(0);
    let mut any_locked = false;
    for (heard, written) in table {
        let locked = store.is_replacement_locked(heard);
        any_locked |= locked;
        let mark = if locked { '*' } else { ' ' };
        println!("{mark} {heard:width$}  {written}");
    }
    if any_locked {
        println!("\n* = set by the system config ({})", system_config(&store));
    }
    Ok(())
}

fn add(heard: &str, written: &str) -> anyhow::Result<()> {
    let mut store = ConfigStore::load()?;
    let heard = unlocked(&store, heard)?;
    store.update(|c| {
        c.replacements.insert(heard.clone(), written.to_owned());
    })?;
    let user = store.paths().user.display();
    println!("replacements.{heard:?} = {written:?} in {user}");
    reload();
    Ok(())
}

fn remove(heard: &str) -> anyhow::Result<()> {
    let mut store = ConfigStore::load()?;
    let heard = unlocked(&store, heard)?;
    anyhow::ensure!(
        store.config().replacements.contains_key(&heard),
        "no replacement for {heard:?}; `earwig replacements list` shows them"
    );
    store.update(|c| {
        c.replacements.remove(&heard);
    })?;
    let user = store.paths().user.display();
    println!("replacements.{heard:?} removed from {user}");
    reload();
    Ok(())
}

/// The phrase as the table keys it, if it has words and the user may set it.
fn unlocked(store: &ConfigStore, heard: &str) -> anyhow::Result<String> {
    let phrase = normalize(heard);
    anyhow::ensure!(
        !phrase.is_empty(),
        "{heard:?} has no words for earwig to hear"
    );
    if store.is_replacement_locked(&phrase) {
        anyhow::bail!(
            "{phrase:?} is set by the system config ({}); change it there",
            system_config(store)
        );
    }
    Ok(phrase)
}

fn system_config(store: &ConfigStore) -> String {
    store
        .paths()
        .system
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

/// A running daemon replaces by the new table from its next dictation.
fn reload() {
    if let Err(e) = crate::daemon::send(&Command::ReloadConfig) {
        log::debug!("daemon not told to reload: {e}");
    }
}
