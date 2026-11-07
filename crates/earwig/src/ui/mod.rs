//! GPUI front end: the dictation overlay, the settings window and the tray.
//!
//! [`run`] owns the main thread. Daemon events arrive on a flume channel and
//! are applied to [`UiState`] (a GPUI global) on the main thread; the overlay
//! and settings windows render from it. Nothing here talks to the microphone,
//! the model or the inserter: every action goes back to the controller as a
//! [`Command`] or a config edit.

mod accent;
mod crt;
mod drag;
mod icon;
mod lifecycle;
mod overlay;
mod pill;
mod platform;
mod settings;
mod water;

use std::sync::PoisonError;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use earwig_core::config::{Config, ThemeSetting};
use earwig_core::controller::Handle;
use earwig_core::desktop::DesktopCaps;
use earwig_core::events::{Command, DictationState, Event, Status};
use gpui_kit::component::{ActiveTheme as _, Theme, ThemeMode};
use gpui_kit::{AnyWindowHandle, App, Context, Entity, Global, QuitMode};

use overlay::OverlayView;

/// Requests that do not come from the daemon (tray clicks).
enum Local {
    ShowSettings,
}

/// How the overlay can float above other windows without taking focus;
/// [`platform::overlay_surface`] picks one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OverlaySurface {
    /// `zwlr_layer_shell_v1` (niri, sway, Hyprland, KDE, COSMIC).
    #[cfg(target_os = "linux")]
    LayerShell,
    /// A borderless popup window that [`platform::Popup`] moves: an
    /// override-redirect X11 window, through XWayland on GNOME; a
    /// non-activating topmost tool window on Windows.
    Popup,
    /// Nothing suitable; showing a normal window would steal focus from the
    /// app the text is meant for, so no overlay.
    None,
}

/// Run the GPUI application on the main thread until the daemon quits.
pub fn run(handle: Handle, caps: DesktopCaps, open_settings: bool) {
    let events = handle.subscribe();
    let voice = handle.spectrum();
    let (local_tx, local_rx) = flume::unbounded();
    let surface = platform::overlay_surface(&caps);

    platform::application(surface, &caps)
        .with_assets(gpui_kit::assets::Assets)
        .with_quit_mode(QuitMode::Explicit)
        .run(move |cx: &mut App| {
            gpui_kit::init(cx);
            // Inside the application: some trays need its run loop.
            let tray = platform::spawn_tray(handle.clone(), local_tx);
            accent::watch(cx);
            if surface == OverlaySurface::None {
                log::warn!(
                    "this desktop offers no way to float the overlay without taking focus; \
                     the overlay stays hidden"
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
                overlay_wake: None,
                input_devices: Vec::new(),
            });
            apply_theme(cx);

            cx.spawn(async move |cx| {
                while let Ok(event) = events.recv_async().await {
                    cx.update(|cx| apply(event, cx));
                }
                log::info!("daemon event feed closed; quitting the UI");
                cx.update(|cx| cx.quit());
            })
            .detach();

            // A voice shows on the next frame, however slowly the overlay
            // was pacing itself; wake-ups coalesce, so this never queues.
            cx.spawn(async move |cx| {
                let wakes = voice.wakes();
                while wakes.recv_async().await.is_ok() {
                    cx.update(|cx| {
                        if let Some(view) = cx.global::<UiState>().overlay.clone()
                            && view.read(cx).hears()
                        {
                            view.update(cx, |_, cx| cx.notify());
                        }
                    });
                }
            })
            .detach();

            cx.spawn(async move |cx| {
                while let Ok(Local::ShowSettings) = local_rx.recv_async().await {
                    cx.update(show_settings);
                }
            })
            .detach();

            if open_settings {
                show_settings(cx);
            }
        });
}

/// Wall-clock milliseconds since the Unix epoch, the unit of daemon timestamps.
pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// Everything the windows render from, owned by the main thread.
pub(crate) struct UiState {
    handle: Handle,
    caps: DesktopCaps,
    surface: OverlaySurface,
    status: Status,
    tray: Option<platform::TrayHandle>,
    overlay: Option<Entity<OverlayView>>,
    settings: Option<AnyWindowHandle>,
    /// When the overlay is woken next to move on by itself (see
    /// [`follow_overlay`]).
    overlay_wake: Option<Instant>,
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

/// Theme every window with the configured scheme. Cheap when it is already
/// in place, so it runs whenever a window opens and picks up hand edits.
pub(crate) fn apply_theme(cx: &mut App) {
    let theme = UiState::handle(cx)
        .config()
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .config()
        .appearance
        .theme;
    let mode = match theme {
        ThemeSetting::Dark => ThemeMode::Dark,
        ThemeSetting::Light => ThemeMode::Light,
    };
    if cx.theme().mode != mode {
        Theme::change(mode, None, cx);
    }
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
        // For socket clients: the overlay follows the faster spectrum feed,
        // and the pill switched off when the recording stopped.
        Event::Level { .. } | Event::Inserted { .. } => {}
        Event::Transcript {
            committed,
            tentative,
        } => update_overlay(cx, |o, _| o.set_text(committed, tentative)),
        Event::InsertFailed { on_clipboard, .. } => {
            update_overlay(cx, |o, _| o.failed(on_clipboard, Instant::now()));
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
        DictationState::Recording { started_at_ms, .. } => {
            // The window of a pill still switching off or saying a failure
            // is reused, unless the pill was dragged: a new dictation
            // starts at home.
            if let Some(view) = cx.global::<UiState>().overlay.clone()
                && view.read(cx).moved_before(started_at_ms)
            {
                close_overlay(cx);
            }
            if cx.global::<UiState>().overlay.is_none() {
                open_overlay(cx);
            }
            update_overlay(cx, |o, _| o.recording(started_at_ms));
        }
        DictationState::Finalizing => update_overlay(cx, |o, cx| o.finish(Instant::now(), cx)),
        DictationState::Idle => update_overlay(cx, |o, _| o.idle()),
    }
}

fn update_overlay(cx: &mut App, f: impl FnOnce(&mut OverlayView, &mut Context<OverlayView>)) {
    if let Some(view) = cx.global::<UiState>().overlay.clone() {
        view.update(cx, |view, cx| {
            f(view, cx);
            cx.notify();
        });
        follow_overlay(cx);
    }
}

/// Close the overlay once it is done, or wake it when it next moves on by
/// itself (a switch-off or a failure notice ending).
fn follow_overlay(cx: &mut App) {
    let Some(view) = cx.global::<UiState>().overlay.clone() else {
        return;
    };
    let (closed, deadline) = {
        let view = view.read(cx);
        (view.is_closed(), view.deadline())
    };
    if closed {
        close_overlay(cx);
        return;
    }
    let state = cx.global_mut::<UiState>();
    let Some(at) = deadline.filter(|&at| state.overlay_wake != Some(at)) else {
        return;
    };
    state.overlay_wake = Some(at);
    cx.spawn(async move |cx| {
        let wait = at.saturating_duration_since(Instant::now());
        cx.background_executor().timer(wait).await;
        cx.update(|cx| {
            let state = cx.global_mut::<UiState>();
            if state.overlay_wake == Some(at) {
                state.overlay_wake = None;
            }
            // A wake-up meant for an earlier pill finds nothing due.
            update_overlay(cx, |o, _| o.advance(Instant::now()));
        });
    })
    .detach();
}

fn open_overlay(cx: &mut App) {
    let state = cx.global::<UiState>();
    let overlay_config = config(&state.handle).overlay;
    let (surface, voice) = (state.surface, state.handle.spectrum());
    if !overlay_config.enabled || surface == OverlaySurface::None {
        return;
    }
    apply_theme(cx);
    match overlay::open(&overlay_config, surface, voice, cx) {
        Ok(view) => cx.global_mut::<UiState>().overlay = Some(view),
        Err(e) => log::error!("opening the overlay: {e:#}"),
    }
}

fn close_overlay(cx: &mut App) {
    if let Some(view) = cx.global_mut::<UiState>().overlay.take() {
        for window in view.read(cx).windows() {
            let _ = window.update(cx, |_, window, _| window.remove_window());
        }
    }
}

/// The pill's × was clicked: drop the dictation (nothing is inserted) and
/// get out of the way at once.
pub(crate) fn discard(cx: &mut App) {
    UiState::handle(cx).send(Command::Cancel);
    close_overlay(cx);
}

fn show_settings(cx: &mut App) {
    refresh_input_devices(cx);
    apply_theme(cx);
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
