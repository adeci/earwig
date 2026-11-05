//! StatusNotifierItem tray icon (ksni, blocking API on its own thread).

use earwig_core::controller::Handle;
use earwig_core::events::{Command, DictationState};
use ksni::blocking::TrayMethods as _;
use ksni::menu::{MenuItem, StandardItem};

use super::Local;

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

/// The app icon drawn at `size` px (ARGB32, network byte order): a filled
/// circle with three sound bars, red while dictating. Mirrors
/// `assets/io.github.adeci.earwig.svg`.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::cast_possible_wrap
)]
fn icon(size: u32, recording: bool) -> ksni::Icon {
    let (r, g, b) = if recording {
        (0xe5, 0x48, 0x4d)
    } else {
        (0x2b, 0x6c, 0xb0)
    };
    let s = size as f32;
    let center = s / 2.;
    let radius = s / 2. - 0.5;
    // Bars in a 24-unit design grid: x centre, half height.
    let bars = [(8.0_f32, 3.5_f32), (12., 6.5), (16., 3.5)];
    let unit = s / 24.;
    let bar_half_width = 1.25 * unit;
    let mut data = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            let dist = ((px - center).powi(2) + (py - center).powi(2)).sqrt();
            let coverage = (radius - dist + 0.5).clamp(0., 1.);
            let bar = bars
                .iter()
                .map(|&(bx, half)| {
                    let dx = bar_half_width - (px - bx * unit).abs();
                    // Rounded ends: distance to the bar's centre segment.
                    let dy = (py - center).abs() - half * unit;
                    let edge = if dy > 0. {
                        bar_half_width - (dy.powi(2) + (px - bx * unit).powi(2)).sqrt()
                    } else {
                        dx
                    };
                    (edge + 0.5).clamp(0., 1.)
                })
                .fold(0., f32::max);
            let mix = |c: u8| (f32::from(c) * (1. - bar) + 255. * bar).round() as u8;
            data.extend_from_slice(&[(coverage * 255.).round() as u8, mix(r), mix(g), mix(b)]);
        }
    }
    ksni::Icon {
        width: size as i32,
        height: size as i32,
        data,
    }
}
