//! The area of the output a layer surface may cover: the output less the
//! bars' exclusive zones.
//!
//! Layer-shell tells a client that only in the configure of a surface it
//! stretches over the area, and GPUI cannot open a zero-sized surface on a
//! scaled output (it sends an invalid viewport). So the overlay asks with a
//! layer surface of its own that never gets a buffer and never shows.

use anyhow::Context as _;
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_compositor::WlCompositor;
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, Dispatch, QueueHandle, delegate_noop};
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1::{
    Layer, ZwlrLayerShellV1,
};
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_surface_v1::{
    self, Anchor, ZwlrLayerSurfaceV1,
};

/// Width and height, in logical pixels, of the usable area of the output the
/// compositor puts new overlay surfaces on (the focused one).
pub fn usable_area() -> anyhow::Result<(u32, u32)> {
    let conn = Connection::connect_to_env()?;
    let (globals, mut queue) = registry_queue_init::<Probe>(&conn)?;
    let qh = queue.handle();
    let compositor: WlCompositor = globals.bind(&qh, 1..=4, ())?;
    let shell: ZwlrLayerShellV1 = globals.bind(&qh, 1..=4, ())?;
    let surface = compositor.create_surface(&qh, ());
    let layer = shell.get_layer_surface(&surface, None, Layer::Overlay, "earwig".into(), &qh, ());
    layer.set_anchor(Anchor::Top | Anchor::Bottom | Anchor::Left | Anchor::Right);
    layer.set_size(0, 0);
    surface.commit();
    let mut probe = Probe::default();
    while probe.size.is_none() && !probe.closed {
        queue.blocking_dispatch(&mut probe)?;
    }
    layer.destroy();
    surface.destroy();
    conn.flush()?;
    probe
        .size
        .context("the compositor closed the probe surface unconfigured")
}

#[derive(Default)]
struct Probe {
    size: Option<(u32, u32)>,
    closed: bool,
}

impl Dispatch<WlRegistry, GlobalListContents> for Probe {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: <WlRegistry as wayland_client::Proxy>::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwlrLayerSurfaceV1, ()> for Probe {
    fn event(
        probe: &mut Self,
        _: &ZwlrLayerSurfaceV1,
        event: zwlr_layer_surface_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_layer_surface_v1::Event::Configure { width, height, .. } => {
                probe.size = Some((width, height));
            }
            zwlr_layer_surface_v1::Event::Closed => probe.closed = true,
            _ => {}
        }
    }
}

delegate_noop!(Probe: WlCompositor);
delegate_noop!(Probe: ignore WlSurface);
delegate_noop!(Probe: ZwlrLayerShellV1);
