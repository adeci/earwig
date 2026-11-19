# gpui-component 0.9.0, patched

The crates.io release with one fix for text fields: a click anywhere in an
`Input`'s box puts the caret in it.

`Input` gives its frame a focus handle of its own, apart from the editor's
(so the focus ring stays on while a prefix or suffix button has focus), and
GPUI focuses an element's tracked handle on a press in it. A press on the
frame's padding or border (10 px at the sides, 8 px above and below, at the
default size) focused the frame: the ring showed, but keys and text go to the
editor's handle only, so nothing typed landed.

- `Input::render` (`src/input/input.rs`): a press on the frame that nothing
  inside it took (the text, a button) focuses the editor and prevents the
  default, so the frame never takes focus from a press.

Unfixed upstream as of 0.10.0; drop this copy once a release has the fix.

Diff against the release: `diff -ru <(cargo registry copy) vendor/gpui-component`.
