//! The tray icon through `tray-icon`: the notification area on Windows, the
//! menu bar on macOS.
//! It lives on the main thread, inside GPUI's run loop, which also pumps its
//! messages; clicks go straight to the daemon or the UI's channel.

use earwig_core::controller::Handle;
use earwig_core::events::{Command, DictationState};
use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};
#[cfg(windows)]
use tray_icon::{MouseButton, MouseButtonState, TrayIconEvent};

use crate::ui::{Local, icon};

/// Pixels of the icon handed to the OS, which scales it to the tray.
const ICON_SIZE: u32 = if cfg!(target_os = "macos") { 36 } else { 32 };

/// Updates the tray from the UI thread.
pub(crate) struct TrayHandle {
    tray: TrayIcon,
    toggle: MenuItem,
    idle: Icon,
    busy: Icon,
}

impl TrayHandle {
    pub(crate) fn set_state(&self, state: DictationState) {
        let idle = matches!(state, DictationState::Idle);
        let (icon, toggle, tooltip) = if idle {
            (&self.idle, "Start dictation", "earwig: ready")
        } else if matches!(state, DictationState::Recording { .. }) {
            (&self.busy, "Stop dictation", "earwig: recording")
        } else {
            (&self.busy, "Stop dictation", "earwig: finishing")
        };
        // On macOS the idle icon is a template, drawn in the menu bar's own
        // colour; the dictating one stays red.
        #[cfg(target_os = "macos")]
        let set = if idle {
            self.tray.set_icon_templated(Some(icon.clone()))
        } else {
            self.tray.set_icon(Some(icon.clone()))
        };
        #[cfg(not(target_os = "macos"))]
        let set = self.tray.set_icon(Some(icon.clone()));
        if let Err(e) = set {
            log::debug!("updating the tray icon: {e}");
        }
        let _ = self.tray.set_tooltip(Some(tooltip));
        self.toggle.set_text(toggle);
    }
}

/// Start the tray; `None` (logged) when the OS has none. Call from inside the
/// GPUI application, on the main thread.
pub(crate) fn spawn(handle: Handle, local: flume::Sender<Local>) -> Option<TrayHandle> {
    match build(handle, local) {
        Ok(tray) => Some(tray),
        Err(e) => {
            log::info!("no tray icon: {e:#}");
            None
        }
    }
}

fn build(handle: Handle, local: flume::Sender<Local>) -> anyhow::Result<TrayHandle> {
    let idle = picture(false)?;
    let busy = picture(true)?;
    let settings = MenuItem::new("Settings", true, None);
    let toggle = MenuItem::new("Start dictation", true, None);
    let quit = MenuItem::new("Quit", true, None);
    let menu = Menu::new();
    menu.append_items(&[&settings, &toggle, &PredefinedMenuItem::separator(), &quit])?;
    let ids: [MenuId; 3] = [
        settings.id().clone(),
        toggle.id().clone(),
        quit.id().clone(),
    ];
    let clicked = local.clone();
    #[cfg(not(windows))]
    drop(local);
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        if event.id == ids[0] {
            let _ = clicked.send(Local::ShowSettings);
        } else if event.id == ids[1] {
            handle.send(Command::Toggle);
        } else if event.id == ids[2] {
            handle.send(Command::Quit);
        }
    }));
    // On Windows a left click opens the settings, as on Linux, and the menu
    // is on the right button; a macOS status item opens its menu.
    #[cfg(windows)]
    TrayIconEvent::set_event_handler(Some(move |event| {
        if let TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        } = event
        {
            let _ = local.send(Local::ShowSettings);
        }
    }));
    let builder = TrayIconBuilder::new()
        .with_id("earwig")
        .with_tooltip("earwig: ready");
    #[cfg(target_os = "macos")]
    let builder = builder.with_icon_templated(idle.clone());
    #[cfg(not(target_os = "macos"))]
    let builder = builder.with_icon(idle.clone());
    let tray = builder
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(cfg!(target_os = "macos"))
        .build()?;
    Ok(TrayHandle {
        tray,
        toggle,
        idle,
        busy,
    })
}

fn picture(recording: bool) -> anyhow::Result<Icon> {
    Ok(Icon::from_rgba(
        icon::rgba(ICON_SIZE, recording),
        ICON_SIZE,
        ICON_SIZE,
    )?)
}
