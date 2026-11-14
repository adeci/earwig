//! Our light theme (soft off-whites, near-black text); the dark one is gpui-component's.

use std::rc::Rc;

use gpui_kit::component::{ActiveTheme as _, Theme, ThemeConfig, ThemeConfigColors, ThemeRegistry};
use gpui_kit::{App, Hsla, SharedString};

/// The settings page, a cool off-white.
const PAGE: u32 = 0x00f3_f4f7;
/// Sidebar and title bar.
const SIDEBAR: u32 = 0x00e9_ecf0;
/// Fields, menus, tooltips and the overlay.
const CARD: u32 = 0x00fb_fbfd;
/// Text, switches, sliders and checks.
const TEXT: u32 = 0x001f_2328;
/// Descriptions, hints and headings.
const MUTED_TEXT: u32 = 0x0059_636e;
const DANGER: u32 = 0x00c4_232d;
/// An ochre that reads as text, unlike the default yellow.
const WARNING: u32 = 0x008a_5c00;
const BORDER: u32 = 0x00da_dee4;
/// Field edges, and switches that are off.
const FIELD_BORDER: u32 = 0x00cd_d2da;
/// Hovered rows, menu items and buttons.
const HOVER: u32 = 0x00e6_e9ee;
/// The open page in the sidebar, and tags.
const SELECTED: u32 = 0x00dc_e0e7;
const RING: u32 = 0x008c_95a1;
const THUMB: u32 = 0x00a3_aab4;
/// The model in use (gpui-component lays it on at 20%).
const IN_USE: u32 = 0x00bf_dbfe;
/// Selected text (laid on at 30%).
const SELECTION: u32 = 0x0055_a0fc;

/// Swap in our light theme, then reload the current one (init loaded the stock theme).
pub(crate) fn install(cx: &mut App) {
    let light = light(ThemeRegistry::global(cx).default_light_theme());
    Theme::global_mut(cx).light_theme = Rc::new(light);
    let mode = cx.theme().mode;
    Theme::change(mode, None, cx);
}

/// The settings page: a shade under the fields on light, the background on dark.
pub(crate) fn page(cx: &App) -> Hsla {
    let theme = cx.theme();
    if theme.is_dark() {
        theme.background
    } else {
        gpui_kit::rgb(PAGE).into()
    }
}

/// The stock light theme in our colours; gpui-component derives the rest.
fn light(default: &ThemeConfig) -> ThemeConfig {
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
    // No track; the fallback would be a band of field colour.
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

    /// WCAG contrast ratio.
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
        // Hover is the darkest surface text sits on.
        for surface in [PAGE, SIDEBAR, CARD, HOVER] {
            for ink in [TEXT, MUTED_TEXT, DANGER, WARNING] {
                let ratio = contrast(ink, surface);
                assert!(ratio >= 4.5, "#{ink:06x} on #{surface:06x}: {ratio:.2}:1");
            }
        }
    }
}
