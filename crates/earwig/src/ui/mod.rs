//! GPUI front end: the dictation overlay, the settings window and the tray.
//!
//! [`run`] owns the main thread. Daemon events arrive on a flume channel and
//! are applied to [`UiState`] (a GPUI global) on the main thread; the overlay
//! and settings windows render from it. Nothing here talks to the microphone,
//! the model or the inserter: every action goes back to the controller as a
//! [`Command`] or a config edit.

mod overlay;
mod settings;
mod tray;

use std::sync::PoisonError;
use std::time::Duration;

use earwig_core::config::Config;
use earwig_core::controller::Handle;
use earwig_core::desktop::DesktopCaps;
use earwig_core::events::{Command, DictationState, Event, Status};
use gpui_kit::component::Theme;
use gpui_kit::{
    AnyWindowHandle, App, AppContext as _, Entity, Global, QuitMode, WindowingModes,
};

use overlay::{Outcome, OverlayView};

/// How long the success flash stays up after the text was inserted.
const SUCCESS_FLASH: Duration = Duration::from_millis(600);
/// The failure flash carries a sentence to read, so it stays a little longer.
const FAILURE_FLASH: Duration = Duration::from_millis(1800);

/// Requests that do not come from the daemon (tray clicks).
enum Local {
    ShowSettings,
}

/// How the overlay can float above other windows without taking focus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OverlaySurface {
    /// `zwlr_layer_shell_v1` (niri, sway, Hyprland, KDE, COSMIC).
    LayerShell,
    /// Override-redirect X11 window, through XWayland on GNOME.
    X11Popup,
    /// Nothing suitable; showing a normal window would steal focus from the
    /// app the text is meant for, so no overlay.
    None,
}

impl OverlaySurface {
    fn for_desktop(caps: &DesktopCaps) -> Self {
        if caps.layer_shell {
            Self::LayerShell
        } else if caps.x11 {
            Self::X11Popup
        } else {
            Self::None
        }
    }
}

/// Run the GPUI application on the main thread until the daemon quits.
// `Result` is the entry-point contract the daemon calls; GPUI itself has no
// fallible startup to report today.
#[allow(clippy::unnecessary_wraps)]
pub fn run(handle: Handle, caps: DesktopCaps) -> anyhow::Result<()> {
    let events = handle.subscribe();
    let (local_tx, local_rx) = flume::unbounded();
    let tray = tray::spawn(handle.clone(), local_tx);
    let surface = OverlaySurface::for_desktop(&caps);
    let app = if surface == OverlaySurface::X11Popup && caps.wayland {
        // No layer-shell (GNOME): run on XWayland, where an override-redirect
        // popup floats above other windows without taking focus. A Wayland
        // toplevel would be centred by the compositor and steal focus.
        gpui_kit::platform::linux(WindowingModes::X11)
    } else {
        gpui_kit::application()
    };

    app.with_assets(gpui_kit::assets::Assets)
        .with_quit_mode(QuitMode::Explicit)
        .run(move |cx: &mut App| {
            gpui_kit::init(cx);
            if surface == OverlaySurface::None {
                log::warn!(
                    "this desktop offers neither layer-shell nor X11; the overlay stays hidden"
                );
            }
            cx.set_global(UiState {
                status: handle.status(),
                handle,
                caps,
                surface,
                tray,
                overlay: None,
                settings: None,
                close_generation: 0,
                input_devices: Vec::new(),
            });

            cx.spawn(async move |cx| {
                while let Ok(event) = events.recv_async().await {
                    cx.update(|cx| apply(event, cx));
                }
                log::info!("daemon event feed closed; quitting the UI");
                cx.update(|cx| cx.quit());
            })
            .detach();

            cx.spawn(async move |cx| {
                while let Ok(Local::ShowSettings) = local_rx.recv_async().await {
                    cx.update(show_settings);
                }
            })
            .detach();
        });
    Ok(())
}

/// Everything the windows render from, owned by the main thread.
pub(crate) struct UiState {
    handle: Handle,
    caps: DesktopCaps,
    surface: OverlaySurface,
    status: Status,
    tray: Option<tray::TrayHandle>,
    overlay: Option<(AnyWindowHandle, Entity<OverlayView>)>,
    settings: Option<AnyWindowHandle>,
    /// Bumped whenever a scheduled overlay close becomes stale.
    close_generation: u64,
    input_devices: Vec<String>,
}

impl Global for UiState {}

impl UiState {
    pub(crate) fn handle(cx: &App) -> &Handle {
        &cx.global::<Self>().handle
    }

    pub(crate) fn caps(cx: &App) -> &DesktopCaps {
        &cx.global::<Self>().caps
    }

    pub(crate) fn status(cx: &App) -> &Status {
        &cx.global::<Self>().status
    }

    /// Microphones as of the last time settings opened.
    pub(crate) fn input_devices(cx: &App) -> &[String] {
        &cx.global::<Self>().input_devices
    }
}

/// The current merged settings.
pub(crate) fn config(handle: &Handle) -> Config {
    handle
        .config()
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .config()
        .clone()
}

/// Apply a settings edit: write the user file, then tell the daemon.
pub(crate) fn edit_config(cx: &mut App, edit: impl FnOnce(&mut Config)) {
    let handle = UiState::handle(cx).clone();
    let result = handle
        .config()
        .write()
        .unwrap_or_else(PoisonError::into_inner)
        .update(edit);
    match result {
        Ok(()) => handle.send(Command::ReloadConfig),
        Err(e) => log::error!("saving settings: {e:#}"),
    }
    cx.refresh_windows();
}

fn apply(event: Event, cx: &mut App) {
    match event {
        Event::Status { status } => {
            let state = status.state;
            cx.global_mut::<UiState>().status = status;
            set_state(state, cx);
            cx.refresh_windows();
        }
        Event::State { state } => {
            cx.global_mut::<UiState>().status.state = state;
            set_state(state, cx);
        }
        Event::Level { level } => update_overlay(cx, |o| o.push_level(level)),
        Event::Transcript {
            committed,
            tentative,
        } => update_overlay(cx, |o| o.set_text(committed, tentative)),
        Event::Inserted { .. } => {
            update_overlay(cx, |o| o.finish(Outcome::Inserted));
            schedule_overlay_close(SUCCESS_FLASH, cx);
        }
        Event::InsertFailed { on_clipboard, .. } => {
            update_overlay(cx, |o| o.finish(Outcome::Failed { on_clipboard }));
            schedule_overlay_close(FAILURE_FLASH, cx);
        }
        Event::Model { model } => {
            cx.global_mut::<UiState>().status.model = model;
            cx.refresh_windows();
        }
        Event::Error { message } => log::warn!("daemon: {message}"),
        Event::ShowSettings => show_settings(cx),
    }
}

fn set_state(state: DictationState, cx: &mut App) {
    if let Some(tray) = &cx.global::<UiState>().tray {
        tray.set_state(state);
    }
    match state {
        DictationState::Recording {
            latched,
            started_at_ms,
        } => {
            // A new recording supersedes any pending close; the window of a
            // previous flash is reused.
            cx.global_mut::<UiState>().close_generation += 1;
            if cx.global::<UiState>().overlay.is_none() {
                open_overlay(cx);
            }
            update_overlay(cx, |o| o.recording(latched, started_at_ms));
        }
        DictationState::Finalizing => update_overlay(cx, OverlayView::finalizing),
        DictationState::Idle => {
            let Some((_, view)) = cx.global::<UiState>().overlay.clone() else {
                return;
            };
            let (recording, outcome) = {
                let view = view.read(cx);
                (view.is_recording(), view.has_outcome())
            };
            if recording {
                // Cancelled: nothing is coming, get out of the way.
                close_overlay(cx);
            } else if !outcome {
                // Finalized without a result event yet; give it a moment.
                schedule_overlay_close(SUCCESS_FLASH, cx);
            }
        }
    }
}

fn update_overlay(cx: &mut App, f: impl FnOnce(&mut OverlayView)) {
    if let Some((_, view)) = cx.global::<UiState>().overlay.clone() {
        view.update(cx, |view, cx| {
            f(view);
            cx.notify();
        });
    }
}

fn open_overlay(cx: &mut App) {
    let state = cx.global::<UiState>();
    let overlay_config = config(&state.handle).overlay;
    if !overlay_config.enabled || state.surface == OverlaySurface::None {
        return;
    }
    let options = overlay::overlay_window_options(
        &state.caps,
        &overlay_config,
        cx.primary_display().as_deref(),
    );
    let max_lines = overlay_config.max_lines;
    let anchor = overlay_config.position;
    // No Base root: it paints an opaque background, and the overlay has no
    // inputs or popovers that need one.
    let mut view = None;
    let opened = cx.open_window(options, |window, cx| {
        Theme::sync_system_appearance(Some(window), cx);
        // Click-through: pointer input goes to whatever is below.
        window.set_input_region(Some(&[]));
        let root = cx.new(|cx| OverlayView::new(max_lines, anchor, cx));
        view = Some(root.clone());
        root
    });
    match (opened, view) {
        (Ok(window), Some(view)) => {
            cx.global_mut::<UiState>().overlay = Some((window.into(), view));
        }
        (Err(e), _) => log::error!("opening the overlay: {e:#}"),
        (Ok(_), None) => {}
    }
}

fn close_overlay(cx: &mut App) {
    if let Some((window, _)) = cx.global_mut::<UiState>().overlay.take() {
        let _ = window.update(cx, |_, window, _| window.remove_window());
    }
}

fn schedule_overlay_close(after: Duration, cx: &mut App) {
    let state = cx.global_mut::<UiState>();
    state.close_generation += 1;
    let generation = state.close_generation;
    cx.spawn(async move |cx| {
        cx.background_executor().timer(after).await;
        cx.update(|cx| {
            if cx.global::<UiState>().close_generation == generation {
                close_overlay(cx);
            }
        });
    })
    .detach();
}

fn show_settings(cx: &mut App) {
    refresh_input_devices(cx);
    if let Some(window) = cx.global::<UiState>().settings
        && window
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
    {
        return;
    }
    match settings::open(cx) {
        Ok(window) => cx.global_mut::<UiState>().settings = Some(window),
        Err(e) => log::error!("opening settings: {e:#}"),
    }
}

/// Re-list microphones off the main thread (ALSA enumeration can take a
/// moment); the settings window re-renders when the list arrives.
fn refresh_input_devices(cx: &mut App) {
    cx.spawn(async move |cx| {
        let devices = cx
            .background_executor()
            .spawn(async { earwig_core::audio::input_devices() })
            .await;
        match devices {
            Ok(devices) => cx.update(|cx| {
                cx.global_mut::<UiState>().input_devices = devices;
                cx.refresh_windows();
            }),
            Err(e) => log::warn!("listing microphones: {e:#}"),
        }
    })
    .detach();
}
