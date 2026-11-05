//! earwig: hold or tap a hotkey, speak, and the text lands where you are typing.

mod cli_models;
mod daemon;
mod ui;

use clap::{Parser, Subcommand};
use earwig_core::events::Command;

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the daemon (the default): tray, overlay, hotkeys, dictation.
    Daemon {
        /// Run without any windows or tray (servers, tests).
        #[arg(long)]
        headless: bool,
    },
    /// Hotkey went down (for compositor press bindings).
    Press,
    /// Hotkey went up (for compositor release bindings).
    Release,
    /// Start or stop dictation (for press-only bindings).
    Toggle,
    /// Discard the current recording.
    Cancel,
    /// Open the settings window, starting the daemon if it is not running.
    Settings,
    /// Print the daemon's status as JSON.
    Status,
    /// Stream daemon events as JSON lines.
    Events,
    /// Stop the daemon.
    Quit,
    /// List, search, download and select models.
    Models {
        #[command(subcommand)]
        command: cli_models::ModelsCmd,
    },
    /// Transcribe WAV files (any rate) with the configured model and print
    /// the text; timings go to stderr.
    Transcribe {
        #[arg(required = true)]
        wav: Vec<std::path::PathBuf>,
        /// Override the configured compute backend.
        #[arg(long, value_parser = ["auto", "cpu", "gpu"])]
        backend: Option<String>,
    },
    /// List audio input devices.
    Devices,
}

fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("info,zbus=warn,tracing=warn"),
    )
    .init();
    let cli = Cli::parse();
    match cli.command.unwrap_or(Cmd::Daemon { headless: false }) {
        Cmd::Daemon { headless } => daemon::run(headless, false),
        Cmd::Press => daemon::send(&Command::Press),
        Cmd::Release => daemon::send(&Command::Release),
        Cmd::Toggle => daemon::send(&Command::Toggle),
        Cmd::Cancel => daemon::send(&Command::Cancel),
        Cmd::Settings => daemon::settings(),
        Cmd::Quit => daemon::send(&Command::Quit),
        Cmd::Status => daemon::status(),
        Cmd::Events => daemon::events(),
        Cmd::Models { command } => cli_models::run(command),
        Cmd::Transcribe { wav, backend } => daemon::transcribe_files(&wav, backend.as_deref()),
        Cmd::Devices => daemon::devices(),
    }
}

/// Byte count in decimal units, as the Hugging Face Hub shows them.
fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["kB", "MB", "GB", "TB"];
    if bytes < 1000 {
        return format!("{bytes} B");
    }
    #[allow(clippy::cast_precision_loss)]
    let mut value = bytes as f64 / 1000.0;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    if value >= 100.0 {
        format!("{value:.0} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::human_size;

    #[test]
    fn decimal_sizes() {
        assert_eq!(human_size(999), "999 B");
        assert_eq!(human_size(731_357_568), "731 MB");
        assert_eq!(human_size(1_500_000_000), "1.5 GB");
    }
}
