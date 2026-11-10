# gpui-pre-linux 0.3.11, patched

The crates.io release with one addition for earwig's overlay: a layer
surface can move while it is mapped, so the pill's own window follows a
drag (GPUI sets a layer surface's anchor and margins only when it opens).

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

Diff against the release: `diff -ru <(cargo registry copy) vendor/gpui-pre-linux`.
