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
        Cmd::Settings => {
            #[cfg(windows)]
            allow_foreground();
            daemon::settings()
        }
        Cmd::Quit => daemon::send(&Command::Quit),
        Cmd::Status => daemon::status(),
        Cmd::Events => daemon::events(),
        Cmd::Models { command } => cli_models::run(command),
        Cmd::Transcribe { wav, backend } => daemon::transcribe_files(&wav, backend.as_deref()),
        Cmd::Devices => daemon::devices(),
    }
}

/// Let the daemon bring its settings window to the front. Windows hands the
/// foreground over only with leave from the process that has it, which this
/// command, run from the active terminal, is.
#[cfg(windows)]
fn allow_foreground() {
    use windows_sys::Win32::UI::WindowsAndMessaging::{ASFW_ANY, AllowSetForegroundWindow};
    // SAFETY: no preconditions; it fails, changing nothing, when this
    // process cannot give the foreground away.
    unsafe { AllowSetForegroundWindow(ASFW_ANY) };
}

/// The units of [`human_size`] past bytes.
const SIZE_UNITS: [&str; 4] = [" kB", " MB", " GB", " TB"];

/// Byte count in decimal units, as the Hugging Face Hub shows them.
fn human_size(bytes: u64) -> String {
    decimal(bytes, " B", &SIZE_UNITS)
}

/// `part` of `whole` bytes in `whole`'s unit, "312 of 740 MB": the whole as
/// [`human_size`] writes it, the part rounded down so that it never reads
/// as done before it is.
#[allow(clippy::cast_precision_loss)]
fn human_size_of(part: u64, whole: u64) -> String {
    let part = part.min(whole);
    let Some((divisor, unit, places)) = magnitude(whole, SIZE_UNITS.len()) else {
        return format!("{part} of {whole} B");
    };
    let shift = if places == 0 { 1.0 } else { 10.0 };
    let part = (part as f64 / divisor * shift).floor() / shift;
    let whole = whole as f64 / divisor;
    format!("{part:.places$} of {whole:.places$}{}", SIZE_UNITS[unit])
}

/// `n` followed by `one` below a thousand; above, in thousands, millions
/// and so on (`units`), to three significant digits.
#[allow(clippy::cast_precision_loss)]
fn decimal(n: u64, one: &str, units: &[&str]) -> String {
    match magnitude(n, units.len()) {
        None => format!("{n}{one}"),
        Some((divisor, unit, places)) => format!("{:.places$}{}", n as f64 / divisor, units[unit]),
    }
}

/// How [`decimal`] writes `n` with `units` units: divided by `divisor`, in
/// unit `unit` (0 for thousands), to `places` decimals; `None` below a
/// thousand.
fn magnitude(n: u64, units: usize) -> Option<(f64, usize, usize)> {
    if n < 1000 {
        return None;
    }
    #[allow(clippy::cast_precision_loss)]
    let n = n as f64;
    let mut divisor = 1000.0;
    let mut unit = 0;
    // Up a unit before rounding would show 1000 of this one.
    while n / divisor >= 999.5 && unit < units - 1 {
        divisor *= 1000.0;
        unit += 1;
    }
    // One decimal below a hundred of the unit.
    let places = usize::from(n / divisor < 99.95);
    Some((divisor, unit, places))
}

#[cfg(test)]
mod tests {
    use super::{human_size, human_size_of};

    #[test]
    fn decimal_sizes() {
        assert_eq!(human_size(999), "999 B");
        assert_eq!(human_size(731_357_568), "731 MB");
        assert_eq!(human_size(1_500_000_000), "1.5 GB");
        assert_eq!(human_size(999_600_000), "1.0 GB", "not 1000 MB");
    }

    #[test]
    fn a_part_takes_the_unit_of_its_whole() {
        assert_eq!(human_size_of(312_400_000, 740_363_168), "312 of 740 MB");
        assert_eq!(human_size_of(739_900_000, 740_363_168), "739 of 740 MB");
        assert_eq!(human_size_of(1_460_000_000, 2_185_030_624), "1.4 of 2.2 GB");
        assert_eq!(human_size_of(0, 135_373_280), "0 of 135 MB");
        assert_eq!(human_size_of(9_000, 900), "900 of 900 B");
    }
}
