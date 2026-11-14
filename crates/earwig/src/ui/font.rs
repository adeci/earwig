//! IBM Plex Sans, bundled so every desktop draws the same.

use std::borrow::Cow;
use std::sync::{Arc, LazyLock};

use gpui_kit::component::Theme;
use gpui_kit::{App, FontFeatures};

const FAMILY: &str = "IBM Plex Sans";

/// IBM Plex Sans 3.005 from @ibm/plex-sans@1.1.0 (OFL), only the weights we draw.
const FACES: [&[u8]; 3] = [
    include_bytes!("../../../../assets/fonts/IBMPlexSans-Regular.ttf"),
    include_bytes!("../../../../assets/fonts/IBMPlexSans-Medium.ttf"),
    include_bytes!("../../../../assets/fonts/IBMPlexSans-SemiBold.ttf"),
];

/// Tabular figures, so changing numbers don't jiggle.
pub(crate) static TABULAR: LazyLock<FontFeatures> =
    LazyLock::new(|| FontFeatures(Arc::new(vec![("tnum".into(), 1)])));

/// False (and logged) if GPUI turns the faces down.
pub(crate) fn register(cx: &mut App) -> bool {
    let faces = FACES.iter().map(|&face| Cow::Borrowed(face)).collect();
    cx.text_system()
        .add_fonts(faces)
        .inspect_err(|e| log::error!("loading the bundled font, drawing in the system's: {e:#}"))
        .is_ok()
}

pub(crate) fn name_on_theme(cx: &mut App) {
    Theme::update(cx, |theme| theme.font_family = FAMILY.into());
}

#[cfg(test)]
mod tests {
    use gpui_kit::FontWeight;
    use ttf_parser::{Face, name_id};

    use super::*;

    /// The family a text system files `face` under: its typographic family, if it has one.
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
        let faces: Vec<_> = FACES
            .into_iter()
            .map(|data| {
                let face = Face::parse(data, 0).expect("a font");
                let weight = FontWeight(f32::from(face.weight().to_number()));
                (family(&face), weight)
            })
            .collect();
        let drawn = [FontWeight::NORMAL, FontWeight::MEDIUM, FontWeight::SEMIBOLD];
        assert_eq!(faces, drawn.map(|weight| (Some(FAMILY.to_owned()), weight)));
    }
}
