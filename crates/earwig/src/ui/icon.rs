//! The app icon drawn in code for the tray: a filled circle with three sound
//! bars, red while dictating. Mirrors `assets/io.github.adeci.earwig.svg`.

/// `size`×`size` pixels, RGBA, rows top to bottom, not premultiplied.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)]
pub(crate) fn rgba(size: u32, recording: bool) -> Vec<u8> {
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
            data.extend_from_slice(&[mix(r), mix(g), mix(b), (coverage * 255.).round() as u8]);
        }
    }
    data
}
