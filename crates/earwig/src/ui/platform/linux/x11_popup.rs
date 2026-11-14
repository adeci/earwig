//! Moving the overlay's override-redirect X11 popup, which GPUI only places when it opens.

use anyhow::{Context as _, bail};
use gpui_kit::{Pixels, Point, Window, point, px};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use x11rb::connection::Connection as _;
use x11rb::protocol::xproto::{ConfigureWindowAux, ConnectionExt as _};
use x11rb::rust_connection::RustConnection;

pub struct X11Popup {
    conn: RustConnection,
    window: u32,
    /// Origin last asked for, `None` while the window is where it opened.
    origin: Option<Point<Pixels>>,
}

impl X11Popup {
    pub fn new(window: &Window) -> anyhow::Result<Self> {
        let RawWindowHandle::Xcb(handle) = HasWindowHandle::window_handle(window)?.as_raw() else {
            bail!("the overlay is not an X11 window");
        };
        let (conn, _) = x11rb::connect(None).context("connecting to the X server")?;
        Ok(Self {
            conn,
            window: handle.window.get(),
            origin: None,
        })
    }

    /// Asked of the server: window-relative events lag a window that's moving.
    pub fn pointer(&self, scale: f32) -> Option<Point<Pixels>> {
        let reply = self.conn.query_pointer(self.window).ok()?.reply().ok()?;
        Some(point(
            px(f32::from(reply.root_x) / scale),
            px(f32::from(reply.root_y) / scale),
        ))
    }

    pub fn move_to(&mut self, origin: Point<Pixels>, scale: f32) {
        if self.origin == Some(origin) {
            return;
        }
        self.origin = Some(origin);
        #[allow(clippy::cast_possible_truncation)]
        let device = |value: Pixels| (f32::from(value) * scale).round() as i32;
        let aux = ConfigureWindowAux::new()
            .x(device(origin.x))
            .y(device(origin.y));
        let sent = self
            .conn
            .configure_window(self.window, &aux)
            .map(x11rb::cookie::VoidCookie::ignore_error)
            .and_then(|()| self.conn.flush());
        if let Err(e) = sent {
            log::warn!("moving the overlay: {e}");
        }
    }
}
