//! Daemon assembly and the thin socket clients. (Pipeline slice.)

use std::path::Path;

use earwig_core::events::Command;

pub fn run(_headless: bool) -> anyhow::Result<()> {
    anyhow::bail!("daemon: not assembled yet")
}

pub fn send(_cmd: Command) -> anyhow::Result<()> {
    anyhow::bail!("socket client: not implemented yet")
}

pub fn status() -> anyhow::Result<()> {
    anyhow::bail!("status: not implemented yet")
}

pub fn events() -> anyhow::Result<()> {
    anyhow::bail!("events: not implemented yet")
}

pub fn transcribe_file(_wav: &Path) -> anyhow::Result<()> {
    anyhow::bail!("transcribe: not implemented yet")
}

pub fn devices() -> anyhow::Result<()> {
    anyhow::bail!("devices: not implemented yet")
}
