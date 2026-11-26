# gpui-pre-linux 0.3.11, patched

The crates.io release with two additions for earwig's overlay:

1. Moving a Wayland layer surface while it is mapped, so the pill's own window
   follows a drag (GPUI sets a layer surface's anchor and margins only when it
   opens):
   - `move_layer_surface(surface, anchor, margin, by)` (`src/gpui_linux.rs`,
     `src/linux/wayland/client.rs`): finds the window by its `wl_surface`,
     sets the anchor and margins and commits.
   - `pointer_travel()` (`src/linux/wayland/client.rs`): the sum of the
     pointer's relative motions (`zwp_relative_pointer_v1`, accelerated, as the
     cursor moves), when the compositor offers the protocol. A surface that
     moves under a held button gets pointer positions some compositors give
     relative to where it was at the press (sway, niri) and others relative to
     where it is; the travel since the press is the same everywhere.
   - `WaylandWindowStatePtr::move_layer` (`src/linux/wayland/window.rs`):
     without relative motion, positions are taken as the protocol says, and
     with `shift_pointer` the window shifts those the compositor sent before it
     processed a move, until a `wl_display.sync` sent with it comes back
     (`LayerMoved`).

2. Live X11 RandR monitor geometry (GPUI's `X11Display::new` reads immutable
   initial connection setup roots, going stale after monitor layout changes and
   spanning all monitors as one bounding box):
   - `X11Display::{all, primary, for_window}` (`src/linux/x11/display.rs`,
     `src/linux/x11/client.rs`, `src/linux/x11/window.rs`): queries live
     `RRGetMonitors` (falling back to live `GetGeometry` on the root window),
     refreshes open windows on RandR change events and cross-monitor moves, and
     preserves the X screen index in `HasDisplayHandle` and `X11WindowState::new`.
   - `x11_pointer_position()` (`src/gpui_linux.rs`, `src/linux/x11/client.rs`):
     returns the root pointer position in logical pixels using the client's
     active XCB connection and scale factor, so callers can choose the monitor
     under the pointer.

Diff against the release: `diff -ru <(cargo registry copy) vendor/gpui-pre-linux`.
