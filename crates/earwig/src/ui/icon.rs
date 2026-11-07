//! The tray icon: `assets/tray.svg`, the app icon's pill fattened to fill
//! the square so it holds its own beside other tray icons, with a big
//! recording dot and one bold word bar cut out; blue, red while dictating.
//! resvg draws it at whatever size the tray asks for.

use std::sync::LazyLock;

use resvg::{tiny_skia, usvg};

/// The pill. Only its shape is used; the state gives the colour.
static PILL: LazyLock<usvg::Tree> = LazyLock::new(|| {
    usvg::Tree::from_str(
        include_str!("../../../../assets/tray.svg"),
        &usvg::Options::default(),
    )
    .expect("assets/tray.svg is valid SVG")
});

const IDLE: [u8; 3] = [0x35, 0x84, 0xe4];
const DICTATING: [u8; 3] = [0xe5, 0x48, 0x4d];

/// `size`×`size` pixels, RGBA, rows top to bottom, not premultiplied; clear
/// pixels are black, since some hosts (swaybar) take the data as
/// premultiplied and would tint the panel around the icon otherwise.
#[allow(clippy::cast_precision_loss)]
pub(crate) fn rgba(size: u32, recording: bool) -> Vec<u8> {
    let [r, g, b] = if recording { DICTATING } else { IDLE };
    let mut pixmap = tiny_skia::Pixmap::new(size, size).expect("the tray asks for pixels");
    let scale = size as f32 / PILL.size().width();
    resvg::render(
        &PILL,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    pixmap
        .pixels()
        .iter()
        .flat_map(|pixel| match pixel.alpha() {
            0 => [0; 4],
            alpha => [r, g, b, alpha],
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pixel at `x`, `y` of a `size`×`size` icon.
    fn pixel(icon: &[u8], size: u32, x: u32, y: u32) -> [u8; 4] {
        let at = ((y * size + x) * 4) as usize;
        icon[at..at + 4].try_into().unwrap()
    }

    #[test]
    fn a_blue_pill_red_while_dictating_with_the_dot_cut_out() {
        let size = 64;
        let idle = rgba(size, false);
        let dictating = rgba(size, true);
        assert_eq!(idle.len(), 64 * 64 * 4);
        // Inside the pill past the words, the recording dot's centre, a corner.
        assert_eq!(pixel(&idle, size, 58, 32), [0x35, 0x84, 0xe4, 255]);
        assert_eq!(pixel(&dictating, size, 58, 32), [0xe5, 0x48, 0x4d, 255]);
        assert_eq!(pixel(&idle, size, 16, 32), [0; 4]);
        assert_eq!(pixel(&idle, size, 0, 0), [0; 4]);
    }
}
