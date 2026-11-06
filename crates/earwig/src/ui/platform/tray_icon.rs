//! The tray icon through `tray-icon` (the notification area on Windows).
//! It lives on the main thread, inside GPUI's run loop, which also pumps its
//! messages; clicks go straight to the daemon or the UI's channel.

use earwig_core::controller::Handle;
use earwig_core::events::{Command, DictationState};
use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

use crate::ui::{Local, icon};

/// Pixels of the icon handed to the OS, which scales it to the tray.
const ICON_SIZE: u32 = 32;

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
        if let Err(e) = self.tray.set_icon(Some(icon.clone())) {
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
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        if event.id == ids[0] {
            let _ = clicked.send(Local::ShowSettings);
        } else if event.id == ids[1] {
            handle.send(Command::Toggle);
        } else if event.id == ids[2] {
            handle.send(Command::Quit);
        }
    }));
    // A left click opens the settings, as on Linux; the menu is on the right
    // button.
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
    let tray = TrayIconBuilder::new()
        .with_id("earwig")
        .with_tooltip("earwig: ready")
        .with_icon(idle.clone())
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
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
