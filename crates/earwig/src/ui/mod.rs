//! GPUI front end: renders daemon events, sends every action back as a command or config edit.

mod accent;
mod assets;
mod crt;
mod drag;
mod font;
mod icon;
mod lifecycle;
mod marquee;
mod overlay;
mod pill;
mod platform;
mod settings;
mod theme;
mod water;

use std::sync::PoisonError;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use earwig_core::audio::{InputDevice, InputDevices};
use earwig_core::config::{AppearanceConfig, Config, ThemeSetting};
use earwig_core::controller::Handle;
use earwig_core::desktop::DesktopCaps;
use earwig_core::events::{Command, DictationState, Event, Status};
use gpui_kit::component::{ActiveTheme as _, Theme, ThemeMode};
use gpui_kit::{AnyWindowHandle, App, Context, Entity, Global, QuitMode};

use overlay::OverlayView;

/// Tray clicks, which don't go through the daemon.
enum Local {
    ShowSettings,
}

/// How the overlay floats above other windows without taking focus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OverlaySurface {
    /// niri, sway, Hyprland, KDE, COSMIC.
    #[cfg(target_os = "linux")]
    LayerShell,
    /// Moved by us: override-redirect X11 (GNOME), no-activate window (Windows), panel (macOS).
    Popup,
    /// A normal window would steal focus, so no overlay.
    None,
}

/// Run the GPUI application on the main thread until the daemon quits.
#[cfg_attr(
    not(target_os = "linux"),
    expect(
        clippy::needless_pass_by_value,
        reason = "only Linux keeps the capabilities, for the settings window"
    )
)]
pub fn run(handle: Handle, caps: DesktopCaps, open_settings: bool) {
    let events = handle.subscribe();
    let voice = handle.spectrum();
    let (local_tx, local_rx) = flume::unbounded();
    let surface = platform::overlay_surface(&caps);

    platform::application(surface, &caps)
        .with_assets(assets::Assets)
        .with_quit_mode(QuitMode::Explicit)
        .run(move |cx: &mut App| {
            // Before init, whose theme looks up the system font (IBM Plex Sans on Linux).
            let bundled = font::register(cx);
            gpui_kit::init(cx);
            theme::install(cx);
            if bundled {
                font::name_on_theme(cx);
            }
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
                #[cfg(target_os = "linux")]
                caps,
                surface,
                tray,
                overlay: None,
                settings: None,
                overlay_wake: None,
                input_devices: InputDevices::default(),
                mic_error: None,
            });
            apply_appearance(cx);

            cx.spawn(async move |cx| {
                while let Ok(event) = events.recv_async().await {
                    cx.update(|cx| apply(event, cx));
                }
                log::info!("daemon event feed closed; quitting the UI");
                cx.update(|cx| cx.quit());
            })
            .detach();

            // Show a voice next frame, however slowly the overlay paces itself (wake-ups coalesce).
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

/// Unix epoch ms, like the daemon's timestamps.
pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// What the windows render from (main thread only).
pub(crate) struct UiState {
    handle: Handle,
    #[cfg(target_os = "linux")]
    caps: DesktopCaps,
    surface: OverlaySurface,
    status: Status,
    tray: Option<platform::TrayHandle>,
    overlay: Option<Entity<OverlayView>>,
    settings: Option<AnyWindowHandle>,
    /// When the overlay's next timed wake-up is due.
    overlay_wake: Option<Instant>,
    input_devices: InputDevices,
    /// Last mic failure, cleared once audio flows again.
    mic_error: Option<String>,
}

impl Global for UiState {}

impl UiState {
    pub(crate) fn handle(cx: &App) -> &Handle {
        &cx.global::<Self>().handle
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn caps(cx: &App) -> &DesktopCaps {
        &cx.global::<Self>().caps
    }

    pub(crate) fn status(cx: &App) -> &Status {
        &cx.global::<Self>().status
    }

    /// Microphones as of the last time settings opened.
    pub(crate) fn input_devices(cx: &App) -> &[InputDevice] {
        &cx.global::<Self>().input_devices.devices
    }

    pub(crate) fn mic_error(cx: &App) -> Option<&str> {
        cx.global::<Self>().mic_error.as_deref()
    }

    pub(crate) fn clear_mic_error(cx: &mut App) {
        cx.global_mut::<Self>().mic_error = None;
    }
}

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

/// Cheap when nothing changed, so every window opening calls it (picking up hand edits).
pub(crate) fn apply_appearance(cx: &mut App) {
    let AppearanceConfig { theme, accent } = UiState::handle(cx)
        .config()
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .config()
        .appearance
        .clone();
    let mode = match theme {
        ThemeSetting::Dark => ThemeMode::Dark,
        ThemeSetting::Light => ThemeMode::Light,
    };
    if cx.theme().mode != mode {
        Theme::change(mode, None, cx);
    }
    accent::set(accent, cx);
}

fn apply(event: Event, cx: &mut App) {
    match event {
        Event::Status { status } => {
            // Snapshots can repeat the state, which would restart the overlay's switch-off.
            let state = status.state;
            let changed = UiState::status(cx).state != state;
            cx.global_mut::<UiState>().status = status;
            if changed {
                set_state(state, cx);
            }
            cx.refresh_windows();
        }
        Event::State { state } => {
            cx.global_mut::<UiState>().status.state = state;
            set_state(state, cx);
        }
        // Audio arrived, so the mic works again. (The overlay follows the spectrum feed.)
        Event::Level { .. } => {
            if UiState::mic_error(cx).is_some() {
                UiState::clear_mic_error(cx);
                cx.refresh_windows();
            }
        }
        // Only for socket clients; the pill switched off already.
        Event::Inserted { .. } => {}
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
        Event::Error { message } => {
            log::warn!("daemon: {message}");
            // Mic failures, for the Microphone settings.
            if let Some(error) = message.strip_prefix(earwig_core::audio::MIC_ERROR_PREFIX) {
                cx.global_mut::<UiState>().mic_error = Some(error.to_owned());
                cx.refresh_windows();
            }
        }
        Event::ShowSettings => show_settings(cx),
    }
}

fn set_state(state: DictationState, cx: &mut App) {
    if let Some(tray) = &cx.global::<UiState>().tray {
        tray.set_state(state);
    }
    match state {
        DictationState::Recording { started_at_ms, .. } => {
            // Reuse a pill still on screen, unless it was dragged: new dictations start at home.
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

/// Closes the overlay once done, or schedules a wake-up for its next timed change.
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
            // A stale wake-up just finds nothing due.
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
    apply_appearance(cx);
    match overlay::open(&overlay_config, surface, voice, cx) {
        Ok(view) => cx.global_mut::<UiState>().overlay = Some(view),
        Err(e) => log::error!("opening the overlay: {e:#}"),
    }
}

fn close_overlay(cx: &mut App) {
    if let Some(view) = cx.global_mut::<UiState>().overlay.take()
        && let Some(window) = view.read(cx).window()
    {
        let _ = window.update(cx, |_, window, _| window.remove_window());
    }
}

/// The pill's ×: drop the dictation and close right away.
pub(crate) fn discard(cx: &mut App) {
    UiState::handle(cx).send(Command::Cancel);
    close_overlay(cx);
}

fn show_settings(cx: &mut App) {
    refresh_input_devices(cx);
    apply_appearance(cx);
    // A menu-bar app's window only gets the front and the keyboard once the app is active.
    #[cfg(target_os = "macos")]
    cx.activate(true);
    if let Some(window) = cx.global::<UiState>().settings
        && window
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
    {
        return;
    }
    match settings::open(cx) {
        Ok(window) => {
            // Opening only shows the window; activate it too, as for one already open.
            let _ = window.update(cx, |_, window, _| window.activate_window());
            cx.global_mut::<UiState>().settings = Some(window);
        }
        Err(e) => log::error!("opening settings: {e:#}"),
    }
}

/// Re-lists mics off the main thread (the sound server can be slow), then re-renders.
fn refresh_input_devices(cx: &mut App) {
    let configured = config(UiState::handle(cx)).audio.device;
    cx.spawn(async move |cx| {
        let devices = cx
            .background_executor()
            .spawn(async move { earwig_core::audio::input_devices(configured.as_deref()) })
            .await;
        cx.update(|cx| {
            match devices {
                Ok(devices) => cx.global_mut::<UiState>().input_devices = devices,
                Err(e) => log::warn!("listing microphones: {e:#}"),
            }
            cx.refresh_windows();
        });
    })
    .detach();
}
