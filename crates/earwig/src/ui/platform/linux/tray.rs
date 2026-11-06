//! StatusNotifierItem tray icon (ksni, blocking API on its own thread).

use earwig_core::controller::Handle;
use earwig_core::events::{Command, DictationState};
use ksni::blocking::TrayMethods as _;
use ksni::menu::{MenuItem, StandardItem};

use crate::ui::Local;

/// Updates the tray from the UI thread.
pub struct TrayHandle(ksni::blocking::Handle<Tray>);

impl TrayHandle {
    pub fn set_state(&self, state: DictationState) {
        self.0.update(|tray| tray.state = state);
    }
}

/// Start the tray; `None` (logged) when the session has no tray host.
pub fn spawn(handle: Handle, local: flume::Sender<Local>) -> Option<TrayHandle> {
    let tray = Tray {
        handle,
        local,
        state: DictationState::Idle,
    };
    match tray.spawn() {
        Ok(handle) => Some(TrayHandle(handle)),
        Err(e) => {
            log::info!("no tray icon: {e}");
            None
        }
    }
}

pub struct Tray {
    handle: Handle,
    local: flume::Sender<Local>,
    state: DictationState,
}

impl Tray {
    fn state_text(&self) -> &'static str {
        match self.state {
            DictationState::Idle => "Ready",
            DictationState::Recording { .. } => "Recording",
            DictationState::Finalizing => "Finishing",
        }
    }
}

impl ksni::Tray for Tray {
    fn id(&self) -> String {
        "earwig".into()
    }

    fn title(&self) -> String {
        "earwig".into()
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        let recording = !matches!(self.state, DictationState::Idle);
        [22, 32, 48, 64]
            .into_iter()
            .map(|size| icon(size, recording))
            .collect()
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            title: "earwig".into(),
            description: self.state_text().into(),
            ..ksni::ToolTip::default()
        }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        let _ = self.local.send(Local::ShowSettings);
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let toggle = if matches!(self.state, DictationState::Idle) {
            "Start dictation"
        } else {
            "Stop dictation"
        };
        vec![
            StandardItem {
                label: "Settings".into(),
                activate: Box::new(|tray: &mut Self| {
                    let _ = tray.local.send(Local::ShowSettings);
                }),
                ..StandardItem::default()
            }
            .into(),
            StandardItem {
                label: toggle.into(),
                activate: Box::new(|tray: &mut Self| tray.handle.send(Command::Toggle)),
                ..StandardItem::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Quit".into(),
                activate: Box::new(|tray: &mut Self| tray.handle.send(Command::Quit)),
                ..StandardItem::default()
            }
            .into(),
        ]
    }
}

/// The app icon at `size` px as ARGB32 in network byte order.
#[allow(clippy::cast_possible_wrap)]
fn icon(size: u32, recording: bool) -> ksni::Icon {
    let mut data = crate::ui::icon::rgba(size, recording);
    for pixel in data.as_chunks_mut::<4>().0 {
        pixel.rotate_right(1);
    }
    ksni::Icon {
        width: size as i32,
        height: size as i32,
        data,
    }
}
