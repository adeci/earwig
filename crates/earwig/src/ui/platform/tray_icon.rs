//! The `tray-icon` tray (Windows, macOS), living on the main thread in GPUI's run loop.

use std::cell::Cell;

use earwig_core::controller::Handle;
use earwig_core::events::{Command, DictationState, UpdateState};
use earwig_core::update::release_page;
use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};
#[cfg(windows)]
use tray_icon::{MouseButton, MouseButtonState, TrayIconEvent};

use crate::ui::{Local, icon};

const ICON_SIZE: u32 = if cfg!(target_os = "macos") { 36 } else { 32 };

pub(crate) struct TrayHandle {
    tray: TrayIcon,
    menu: Menu,
    toggle: MenuItem,
    /// "Update to …", at the top of the menu while there is one.
    update: MenuItem,
    update_shown: Cell<bool>,
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
        // On macOS the idle icon is a template the menu bar tints; dictating stays red.
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

    pub(crate) fn set_update(&self, update: Option<&UpdateState>) {
        let item = match update {
            Some(
                UpdateState::Available {
                    version,
                    installable: true,
                }
                | UpdateState::Failed { version, .. },
            ) => Some((format!("Update to {version}"), true)),
            Some(UpdateState::Available {
                version,
                installable: false,
            }) => Some((format!("{version} is available"), true)),
            Some(
                UpdateState::Downloading { version, .. } | UpdateState::Installing { version },
            ) => Some((format!("Updating to {version}…"), false)),
            Some(UpdateState::None) | None => None,
        };
        match item {
            Some((text, enabled)) => {
                self.update.set_text(text);
                self.update.set_enabled(enabled);
                if !self.update_shown.replace(true)
                    && let Err(e) = self.menu.insert(&self.update, 0)
                {
                    log::warn!("adding the update to the tray menu: {e}");
                }
            }
            None => {
                if self.update_shown.replace(false) {
                    let _ = self.menu.remove(&self.update);
                }
            }
        }
    }
}

/// `None` (logged) if the OS has no tray. Call inside the GPUI app, on the main thread.
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
    let update = MenuItem::new("", true, None);
    let menu = Menu::new();
    menu.append_items(&[&settings, &toggle, &PredefinedMenuItem::separator(), &quit])?;
    let ids: [MenuId; 4] = [
        settings.id().clone(),
        toggle.id().clone(),
        quit.id().clone(),
        update.id().clone(),
    ];
    #[cfg(windows)]
    let left_click = local.clone();
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        if event.id == ids[0] {
            let _ = local.send(Local::ShowSettings);
        } else if event.id == ids[1] {
            handle.send(Command::Toggle);
        } else if event.id == ids[2] {
            handle.send(Command::Quit);
        } else if event.id == ids[3] {
            // A copy that can't replace itself points to the release instead.
            match handle.status().update {
                Some(UpdateState::Available {
                    version,
                    installable: false,
                }) => {
                    let _ = local.send(Local::OpenUrl(release_page(&version)));
                }
                _ => handle.send(Command::Update),
            }
        }
    }));
    // Windows: a left click opens settings (as on Linux), the menu is on the right button.
    #[cfg(windows)]
    TrayIconEvent::set_event_handler(Some(move |event| {
        if let TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        } = event
        {
            let _ = left_click.send(Local::ShowSettings);
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
        .with_menu(Box::new(menu.clone()))
        .with_menu_on_left_click(cfg!(target_os = "macos"))
        .build()?;
    Ok(TrayHandle {
        tray,
        menu,
        toggle,
        update,
        update_shown: Cell::new(false),
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
