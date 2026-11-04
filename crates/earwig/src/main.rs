//! earwig: hold or tap a hotkey, speak, and the text lands where you are typing.

mod cli_models;
mod daemon;
mod ui;

use clap::{Parser, Subcommand};

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
        Cmd::Press => daemon::send(&earwig_core::events::Command::Press),
        Cmd::Release => daemon::send(&earwig_core::events::Command::Release),
        Cmd::Toggle => daemon::send(&earwig_core::events::Command::Toggle),
        Cmd::Cancel => daemon::send(&earwig_core::events::Command::Cancel),
        Cmd::Settings => daemon::settings(),
        Cmd::Quit => daemon::send(&earwig_core::events::Command::Quit),
        Cmd::Status => daemon::status(),
        Cmd::Events => daemon::events(),
        Cmd::Models { command } => cli_models::run(command),
        Cmd::Transcribe { wav, backend } => daemon::transcribe_files(&wav, backend.as_deref()),
        Cmd::Devices => daemon::devices(),
    }
}
