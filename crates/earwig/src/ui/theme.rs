//! earwig's light theme: calm off-whites and a soft near-black in place of
//! gpui-component's white and black, in every earwig window. The dark theme
//! is gpui-component's own.
//!
//! gpui-component paints fields and menus with the theme's background, so
//! that is the lightest shade, and the settings page has a colour of its own
//! a shade darker ([`page`]); the sidebar and the title bar are a shade
//! darker again. Text keeps at least 4.5:1 on all of them.

use std::rc::Rc;

use gpui_kit::component::{ActiveTheme as _, Theme, ThemeConfig, ThemeConfigColors, ThemeRegistry};
use gpui_kit::{App, Hsla, SharedString};

/// The settings page: an off-white like Windows 11's light settings, a
/// little cool like GitHub's canvas.
const PAGE: u32 = 0x00f3_f4f7;
/// The sidebar and the title bar, a shade darker than the page.
const SIDEBAR: u32 = 0x00e9_ecf0;
/// Fields, menus, tooltips and the overlay, a touch lighter than the page.
const CARD: u32 = 0x00fb_fbfd;
/// Text, and switches, sliders and checks: GitHub's near-black.
const TEXT: u32 = 0x001f_2328;
/// Descriptions, hints and headings.
const MUTED_TEXT: u32 = 0x0059_636e;
/// Errors, deep enough to read as text.
const DANGER: u32 = 0x00c4_232d;
/// Warnings, an ochre that reads as text where the default yellow cannot.
const WARNING: u32 = 0x008a_5c00;
/// Lines between rows and around boxes.
const BORDER: u32 = 0x00da_dee4;
/// The edge of a field, a little firmer, and a switch that is off.
const FIELD_BORDER: u32 = 0x00cd_d2da;
/// A row, menu item or plain button under the pointer.
const HOVER: u32 = 0x00e6_e9ee;
/// The open page in the sidebar, and tags.
const SELECTED: u32 = 0x00dc_e0e7;
/// The focus ring.
const RING: u32 = 0x008c_95a1;
/// The scrollbar's thumb.
const THUMB: u32 = 0x00a3_aab4;
/// The model in use: gpui-component's pale blue, which it lays on at 20%.
const IN_USE: u32 = 0x00bf_dbfe;
/// Selected text: gpui-component's blue, which it lays on at 30%.
const SELECTION: u32 = 0x0055_a0fc;

/// Make earwig's palette the light theme. `gpui_kit::init` has already
/// loaded gpui-component's own, so reload whichever theme is current.
pub(crate) fn install(cx: &mut App) {
    let light = light(ThemeRegistry::global(cx).default_light_theme());
    Theme::global_mut(cx).light_theme = Rc::new(light);
    let mode = cx.theme().mode;
    Theme::change(mode, None, cx);
}

/// The settings window's page: [`PAGE`] on the light theme, under fields a
/// touch lighter; the background on the dark theme, whose fields are tints.
pub(crate) fn page(cx: &App) -> Hsla {
    let theme = cx.theme();
    if theme.is_dark() {
        theme.background
    } else {
        gpui_kit::rgb(PAGE).into()
    }
}

/// gpui-component's default light theme in earwig's colours. Whatever is
/// not named here gpui-component derives from them.
fn light(default: &ThemeConfig) -> ThemeConfig {
    // `0xRRGGBB` the way a theme file writes it.
    let hex = |rgb: u32| Some(SharedString::from(format!("#{rgb:06x}")));
    let mut colors = ThemeConfigColors::default();
    colors.background = hex(CARD);
    colors.foreground = hex(TEXT);
    colors.muted = hex(SIDEBAR);
    colors.muted_foreground = hex(MUTED_TEXT);
    colors.border = hex(BORDER);
    colors.input = hex(FIELD_BORDER);
    colors.ring = hex(RING);
    colors.primary = hex(TEXT);
    colors.primary_foreground = hex(CARD);
    colors.secondary = hex(SELECTED);
    colors.accent = hex(HOVER);
    colors.list_hover = hex(HOVER);
    colors.list_active = hex(IN_USE);
    colors.selection = hex(SELECTION);
    colors.sidebar = hex(SIDEBAR);
    colors.sidebar_accent = hex(SELECTED);
    colors.title_bar = hex(SIDEBAR);
    colors.switch = hex(FIELD_BORDER);
    // No track: left to the fallback it would be a band of field colour.
    colors.scrollbar = Some("#00000000".into());
    colors.scrollbar_thumb = hex(THUMB);
    colors.danger = hex(DANGER);
    colors.warning = hex(WARNING);
    let mut config = default.clone();
    config.name = "earwig Light".into();
    config.colors = colors;
    config
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The WCAG contrast ratio of two colours.
    fn contrast(a: u32, b: u32) -> f32 {
        let luminance = |rgb: u32| {
            let rgb = gpui_kit::rgb(rgb);
            let linear = |c: f32| {
                if c <= 0.040_45 {
                    c / 12.92
                } else {
                    ((c + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * linear(rgb.r) + 0.7152 * linear(rgb.g) + 0.0722 * linear(rgb.b)
        };
        let (a, b) = (luminance(a), luminance(b));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    #[test]
    fn text_reads_on_every_light_surface() {
        // A hovered row is the darkest surface text sits on; the model in
        // use is a pale wash over the page, lighter than that.
        for surface in [PAGE, SIDEBAR, CARD, HOVER] {
            for ink in [TEXT, MUTED_TEXT, DANGER, WARNING] {
                let ratio = contrast(ink, surface);
                assert!(ratio >= 4.5, "#{ink:06x} on #{surface:06x}: {ratio:.2}:1");
            }
        }
    }
}
