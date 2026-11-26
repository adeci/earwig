use anyhow::Context as _;
use uuid::Uuid;
use x11rb::{
    connection::Connection as _,
    protocol::{randr::ConnectionExt as _, xproto::ConnectionExt as _},
    xcb_ffi::XCBConnection,
};

use gpui::{Bounds, DisplayId, Pixels, PlatformDisplay, Size, point, px};

#[derive(Debug, Clone)]
pub(crate) struct X11Display {
    id: DisplayId,
    bounds: Bounds<Pixels>,
    uuid: Uuid,
    primary: bool,
}

impl X11Display {
    /// Decodes the X screen index packed into a display id.
    pub(crate) fn screen_index(id: DisplayId) -> usize {
        (u64::from(id) >> 32) as usize
    }

    fn make_id(x_screen_index: usize, monitor_key: u32) -> DisplayId {
        DisplayId::new(((x_screen_index as u64) << 32) | u64::from(monitor_key))
    }

    /// Queries live RandR monitors on `x_screen_index`, falling back to live
    /// root window geometry when RandR is unavailable or reports no monitors.
    pub(crate) fn all(
        xcb: &XCBConnection,
        scale_factor: f32,
        x_screen_index: usize,
    ) -> anyhow::Result<Vec<Self>> {
        let screen = xcb
            .setup()
            .roots
            .get(x_screen_index)
            .with_context(|| format!("No screen found with index {x_screen_index}"))?;
        if let Some(monitors) = randr_monitors(xcb, screen.root, scale_factor, x_screen_index) {
            return Ok(monitors);
        }
        let (width, height) = xcb
            .get_geometry(screen.root)
            .ok()
            .and_then(|cookie| cookie.reply().ok())
            .map_or((screen.width_in_pixels, screen.height_in_pixels), |geom| {
                (geom.width, geom.height)
            });
        let id = Self::make_id(x_screen_index, 0);
        Ok(vec![Self {
            id,
            bounds: Bounds {
                origin: Default::default(),
                size: Size {
                    width: px(f32::from(width) / scale_factor),
                    height: px(f32::from(height) / scale_factor),
                },
            },
            uuid: Uuid::from_u128(u128::from(u64::from(id))),
            primary: true,
        }])
    }

    pub(crate) fn primary(
        xcb: &XCBConnection,
        scale_factor: f32,
        x_screen_index: usize,
    ) -> anyhow::Result<Self> {
        let displays = Self::all(xcb, scale_factor, x_screen_index)?;
        Ok(pick_primary(displays))
    }

    /// Picks the live monitor for a window, preferring `preferred_id`, then the
    /// monitor containing or overlapping `bounds`, then the primary monitor.
    pub(crate) fn for_window(
        xcb: &XCBConnection,
        scale_factor: f32,
        x_screen_index: usize,
        preferred_id: Option<DisplayId>,
        bounds: Bounds<Pixels>,
    ) -> anyhow::Result<Self> {
        let displays = Self::all(xcb, scale_factor, x_screen_index)?;
        Ok(pick_for_window(displays, preferred_id, bounds))
    }
}

fn randr_monitors(
    xcb: &XCBConnection,
    root: u32,
    scale_factor: f32,
    x_screen_index: usize,
) -> Option<Vec<X11Display>> {
    let reply = xcb.randr_get_monitors(root, true).ok()?.reply().ok()?;
    let displays: Vec<X11Display> = reply
        .monitors
        .into_iter()
        .enumerate()
        .filter(|(_, monitor)| monitor.width > 0 && monitor.height > 0)
        .map(|(index, monitor)| {
            let key = if monitor.name != 0 {
                monitor.name
            } else {
                (index as u32).saturating_add(1)
            };
            let id = X11Display::make_id(x_screen_index, key);
            X11Display {
                id,
                bounds: Bounds {
                    origin: point(
                        px(f32::from(monitor.x) / scale_factor),
                        px(f32::from(monitor.y) / scale_factor),
                    ),
                    size: Size {
                        width: px(f32::from(monitor.width) / scale_factor),
                        height: px(f32::from(monitor.height) / scale_factor),
                    },
                },
                uuid: Uuid::from_u128(u128::from(u64::from(id))),
                primary: monitor.primary,
            }
        })
        .collect();
    (!displays.is_empty()).then_some(displays)
}

fn pick_primary(displays: Vec<X11Display>) -> X11Display {
    if let Some(pos) = displays.iter().position(|d| d.primary) {
        return displays.into_iter().nth(pos).unwrap();
    }
    displays.into_iter().next().expect("at least one display")
}

fn pick_for_window(
    displays: Vec<X11Display>,
    preferred_id: Option<DisplayId>,
    bounds: Bounds<Pixels>,
) -> X11Display {
    if let Some(id) = preferred_id
        && let Some(pos) = displays.iter().position(|d| d.id == id)
    {
        return displays.into_iter().nth(pos).unwrap();
    }
    let center = bounds.center();
    if let Some(pos) = displays.iter().position(|d| d.bounds.contains(&center)) {
        return displays.into_iter().nth(pos).unwrap();
    }
    if let Some((pos, _)) = displays
        .iter()
        .enumerate()
        .map(|(i, d)| (i, overlap_area(d.bounds, bounds)))
        .filter(|&(_, area)| area > 0.0)
        .max_by(|a, b| a.1.total_cmp(&b.1))
    {
        return displays.into_iter().nth(pos).unwrap();
    }
    pick_primary(displays)
}

fn overlap_area(a: Bounds<Pixels>, b: Bounds<Pixels>) -> f32 {
    let w = (f32::from(a.right()).min(f32::from(b.right()))
        - f32::from(a.left()).max(f32::from(b.left())))
    .max(0.0);
    let h = (f32::from(a.bottom()).min(f32::from(b.bottom()))
        - f32::from(a.top()).max(f32::from(b.top())))
    .max(0.0);
    w * h
}

impl PlatformDisplay for X11Display {
    fn refresh_interval(&self) -> Option<std::time::Duration> {
        None
    }

    fn id(&self) -> DisplayId {
        self.id
    }

    fn uuid(&self) -> anyhow::Result<Uuid> {
        Ok(self.uuid)
    }

    fn bounds(&self) -> Bounds<Pixels> {
        self.bounds
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn display(screen: usize, key: u32, x: f32, y: f32, w: f32, h: f32, primary: bool) -> X11Display {
        let id = X11Display::make_id(screen, key);
        X11Display {
            id,
            bounds: Bounds::new(point(px(x), px(y)), Size { width: px(w), height: px(h) }),
            uuid: Uuid::from_u128(u128::from(u64::from(id))),
            primary,
        }
    }

    #[test]
    fn screen_index_round_trips_through_display_id() {
        for (screen, key) in [(0, 0), (0, 42), (1, 99)] {
            let id = X11Display::make_id(screen, key);
            assert_eq!(X11Display::screen_index(id), screen);
        }
    }

    #[test]
    fn pick_primary_prefers_primary_flag_else_first() {
        let a = display(0, 1, 0., 0., 1920., 1080., false);
        let b = display(0, 2, 1920., 0., 2560., 1440., true);
        assert_eq!(pick_primary(vec![a.clone(), b.clone()]).id(), b.id());

        let c = display(0, 3, 1920., 0., 2560., 1440., false);
        assert_eq!(pick_primary(vec![a.clone(), c]).id(), a.id());
    }

    #[test]
    fn pick_for_window_matches_preferred_id_then_center_then_overlap_then_primary() {
        let left = display(0, 10, 0., 0., 2560., 1440., true);
        let right = display(0, 20, 2560., 0., 1920., 1080., false);
        let monitors = vec![left.clone(), right.clone()];

        let on_left = Bounds::new(point(px(100.), px(100.)), Size { width: px(500.), height: px(200.) });
        assert_eq!(
            pick_for_window(monitors.clone(), Some(right.id()), on_left).id(),
            right.id()
        );

        let on_right = Bounds::new(point(px(2800.), px(400.)), Size { width: px(500.), height: px(200.) });
        assert_eq!(
            pick_for_window(monitors.clone(), None, on_right).id(),
            right.id()
        );

        // Center in dead space below the right monitor, but still overlapping it.
        let straddling_right = Bounds::new(point(px(2800.), px(1000.)), Size { width: px(500.), height: px(400.) });
        assert_eq!(
            pick_for_window(monitors, None, straddling_right).id(),
            right.id()
        );

        // Stale window bounds after unplugging the right monitor fall back to the remaining primary.
        assert_eq!(
            pick_for_window(vec![left.clone()], Some(right.id()), on_right).id(),
            left.id()
        );
    }
}
