//! The UI's typeface, IBM Plex Sans, compiled in so the overlay and the
//! settings window look the same on every desktop, whatever fonts it has.

use std::borrow::Cow;

use gpui_kit::App;
use gpui_kit::component::Theme;

const FAMILY: &str = "IBM Plex Sans";

/// IBM Plex Sans 3.005, unmodified from IBM's `@ibm/plex-sans@1.1.0`
/// release, under the SIL Open Font License
/// (`assets/fonts/LICENSE-IBM-Plex-Sans.txt`). Only the weights the UI
/// draws: regular, medium (the overlay's timer, the open page in the
/// settings sidebar) and semibold (model names); any other weight gets the
/// nearest of them.
const FACES: [&[u8]; 3] = [
    include_bytes!("../../../../assets/fonts/IBMPlexSans-Regular.ttf"),
    include_bytes!("../../../../assets/fonts/IBMPlexSans-Medium.ttf"),
    include_bytes!("../../../../assets/fonts/IBMPlexSans-SemiBold.ttf"),
];

/// Hand GPUI the faces; false, and logged, if it turns them down.
pub(crate) fn register(cx: &mut App) -> bool {
    let faces = FACES.iter().map(|&face| Cow::Borrowed(face)).collect();
    cx.text_system()
        .add_fonts(faces)
        .inspect_err(|e| log::error!("loading the bundled font, drawing in the system's: {e:#}"))
        .is_ok()
}

/// Make the faces the theme's font, which the settings window's root and
/// the overlay's pill draw their text in.
pub(crate) fn name_on_theme(cx: &mut App) {
    Theme::update(cx, |theme| theme.font_family = FAMILY.into());
}

#[cfg(test)]
mod tests {
    use gpui_kit::FontWeight;
    use ttf_parser::{Face, name_id};

    use super::*;

    fn faces() -> impl Iterator<Item = Face<'static>> {
        FACES
            .into_iter()
            .map(|data| Face::parse(data, 0).expect("a font"))
    }

    /// The family a text system files `face` under: its typographic
    /// family, if it names one.
    fn family(face: &Face) -> Option<String> {
        let names = face.names();
        [name_id::TYPOGRAPHIC_FAMILY, name_id::FAMILY]
            .into_iter()
            .find_map(|id| {
                names
                    .into_iter()
                    .find(|name| name.name_id == id && name.is_unicode())?
                    .to_string()
            })
    }

    #[test]
    fn every_weight_the_ui_draws_is_a_face_of_the_theme_font() {
        let faces: Vec<_> = faces()
            .map(|face| {
                let weight = FontWeight(f32::from(face.weight().to_number()));
                (family(&face), weight)
            })
            .collect();
        let drawn = [FontWeight::NORMAL, FontWeight::MEDIUM, FontWeight::SEMIBOLD];
        assert_eq!(faces, drawn.map(|weight| (Some(FAMILY.to_owned()), weight)));
    }

    /// The overlay's timer stays put as it counts.
    #[test]
    fn every_digit_is_as_wide_as_the_others() {
        for face in faces() {
            let widths: Vec<_> = ('0'..='9')
                .map(|digit| {
                    face.glyph_index(digit)
                        .and_then(|glyph| face.glyph_hor_advance(glyph))
                        .expect("a digit")
                })
                .collect();
            assert!(widths.iter().all(|&width| width == widths[0]), "{widths:?}");
        }
    }
}
