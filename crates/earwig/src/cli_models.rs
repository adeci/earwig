//! `earwig models ...` (models slice).

use clap::Subcommand;

#[derive(Subcommand)]
pub enum ModelsCmd {
    /// Curated and installed models.
    List,
}

pub fn run(_cmd: ModelsCmd) -> anyhow::Result<()> {
    anyhow::bail!("models: not implemented yet")
}
