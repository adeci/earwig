# gpui-pre-macos 0.3.11, patched

The crates.io release with one fix for live resizing: while a window is
resized by its edge, frames come only from AppKit's redraw at each new size
(`displayLayer:`), which presents with the transaction that resizes the
window.

Before, the `CAMetalDisplayLink` (macOS 14+) also drew whenever GPUI wanted
a frame (gpui-component's resizable panels ask for one after every size
change), and presented it on its own. That frame reached the screen after
the window had taken another size, and AppKit stretched it to that size, so
the content, text most visibly, wobbled while the window was dragged.

- `metal_frame` (`src/window.rs`): during a live resize, a frame GPUI asked
  for (`MetalFrames::woken`, `src/display_link.rs`) becomes
  `setNeedsDisplay:` on the view instead of a draw into the link's drawable.
- `step` (`src/window.rs`, the `CVDisplayLink` path before macOS 14): the
  same, for every tick during a live resize.

Diff against the release: `diff -ru <(cargo registry copy) vendor/gpui-pre-macos`.
