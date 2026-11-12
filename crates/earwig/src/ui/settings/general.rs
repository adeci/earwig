//! General settings: the hotkey, the microphone, how the overlay and this
//! window look, and how text is typed (Linux) or whether earwig starts at
//! sign-in (Windows).

use std::iter;
use std::sync::PoisonError;

#[cfg(target_os = "linux")]
use earwig_core::config::InsertSetting;
use earwig_core::config::{
    ActivationMode, Config, OverlayAnimation, OverlayPosition, ThemeSetting,
};
#[cfg(target_os = "linux")]
use earwig_core::events::KeyboardState;
use gpui_kit::component::label::Label;
use gpui_kit::component::setting::{
    NumberFieldOptions, SettingField, SettingGroup, SettingItem, SettingPage,
};
use gpui_kit::component::{ActiveTheme as _, IconName, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{App, Axis, Entity, ParentElement as _, SharedString, Styled as _, div, px};
#[cfg(target_os = "linux")]
use gpui_kit::{HighlightStyle, InteractiveText, IntoElement, StyledText, UnderlineStyle};
use serde::Serialize;
use serde::de::DeserializeOwned;

use super::pill_preview::PillPreview;
use super::shortcut::{self, ShortcutField};
use super::{hinted, is_locked, item, section, spaced};
use crate::ui::{UiState, apply_appearance, config, edit_config};

/// Search words for the Hotkey group's rows, whose titles do not say them.
const HOTKEY_KEYWORDS: [&str; 6] = ["hotkey", "shortcut", "key", "bind", "record", "keyboard"];

/// How to bind the hotkey on each Linux desktop.
#[cfg(target_os = "linux")]
const SETUP_GUIDE: &str = "https://github.com/adeci/earwig/blob/main/docs/desktops.md";

pub fn page(
    pill: &Entity<PillPreview>,
    shortcut: &Entity<ShortcutField>,
    settings: &Config,
    cx: &App,
) -> SettingPage {
    let mut groups = Vec::new();
    if let Some(error) = user_error(cx) {
        groups.push(section(
            "Settings file",
            None,
            [SettingItem::render(move |_, _, cx| {
                div().text_sm().text_color(cx.theme().danger).child(format!(
                    "Your settings file has an error, so earwig ignores it and saves no \
                     changes.\n{error}"
                ))
            })],
        ));
    }
    #[cfg(target_os = "macos")]
    let permissions = earwig_core::permissions::check();
    #[cfg(target_os = "macos")]
    let missing = permissions
        .iter()
        .any(|status| status.access != earwig_core::permissions::Access::Granted);
    #[cfg(target_os = "macos")]
    if missing {
        groups.push(permissions_group(&permissions));
    }
    groups.extend([
        hotkey(settings, shortcut, cx),
        microphone(settings, cx),
        appearance(pill, cx),
    ]);
    #[cfg(target_os = "linux")]
    groups.push(typing(cx));
    #[cfg(any(windows, target_os = "macos"))]
    groups.push(startup());
    #[cfg(target_os = "macos")]
    if !missing {
        groups.push(permissions_group(&permissions));
    }
    SettingPage::new("General")
        .icon(IconName::Settings2)
        .resettable(false)
        .groups(spaced(groups))
}

/// The hotkey: the shortcut earwig watches, recorded by pressing it, and
/// what a press does. On Linux, where earwig may not read the keyboard yet,
/// how to let it and what works meanwhile.
fn hotkey(settings: &Config, shortcut: &Entity<ShortcutField>, cx: &App) -> SettingGroup {
    let status = earwig_core::hotkey::status();
    let label = (status.source == "keyboard").then(|| shortcut::label(&settings.hotkey.key));
    let mode = item(
        cx,
        "hotkey.mode",
        "Mode",
        &mode_description(settings.hotkey.mode, label.as_deref()),
        enum_dropdown(
            &[
                (ActivationMode::Hold, "Push to talk"),
                (ActivationMode::Toggle, "Toggle"),
                (ActivationMode::HoldOrToggle, "Both"),
            ],
            |c| c.hotkey.mode,
            |c, v| c.hotkey.mode = v,
        ),
    );
    let rows = [shortcut::item(shortcut, &settings.hotkey.key, cx), mode].into_iter();
    #[cfg(target_os = "linux")]
    let rows = rows.chain(linux_rows(&status, shortcut, cx));
    section(
        "Hotkey",
        None,
        rows.map(|row| row.keywords(HOTKEY_KEYWORDS)),
    )
}

/// Without keyboard access, how to give it; while earwig does not watch the
/// shortcut itself, what works meanwhile.
#[cfg(target_os = "linux")]
fn linux_rows(
    status: &earwig_core::events::HotkeyStatus,
    shortcut: &Entity<ShortcutField>,
    cx: &App,
) -> Vec<SettingItem> {
    let mut rows = Vec::new();
    if status.keyboard == KeyboardState::NoAccess {
        rows.push(shortcut::access_item(shortcut));
    }
    if status.source != "keyboard" {
        let help = fallback(&UiState::caps(cx).desktop, status.source == "portal");
        rows.push(SettingItem::render(move |_, _, cx| help_line(&help, cx)));
    }
    rows
}

/// What a press does in `mode`, naming the key where earwig watches it.
fn mode_description(mode: ActivationMode, key: Option<&str>) -> String {
    let key = key.unwrap_or("the hotkey");
    match mode {
        ActivationMode::Hold => format!("Hold {key} while you talk."),
        ActivationMode::Toggle => format!("Press {key} to start and stop."),
        ActivationMode::HoldOrToggle => format!("Hold or tap {key} to talk."),
    }
}

/// What works on Linux while earwig does not watch the shortcut itself.
#[cfg(target_os = "linux")]
#[derive(Debug, PartialEq)]
enum Fallback {
    Text(SharedString),
    /// "Bind a key in your compositor", linking the setup guide.
    SetupGuide,
}

/// The shortcut the desktop's GlobalShortcuts portal bound for earwig
/// (`portal`), changed in the desktop's settings; anywhere else a key bound
/// in the compositor's config, as the setup guide shows.
#[cfg(target_os = "linux")]
fn fallback(desktop: &str, portal: bool) -> Fallback {
    let desktops: Vec<&str> = desktop.split(':').collect();
    let is = |name: &str| desktops.iter().any(|d| d.eq_ignore_ascii_case(name));
    let works = |place: &str| {
        Fallback::Text(format!("Meanwhile, the shortcut set in {place} works.").into())
    };
    if !portal {
        Fallback::SetupGuide
    } else if is("kde") {
        works("System Settings → Keyboard → Shortcuts")
    } else if is("gnome") {
        works("Settings → Keyboard → Shortcuts")
    } else {
        works("the desktop's keyboard settings")
    }
}

/// `help` as a line of small text, the setup guide a link.
#[cfg(target_os = "linux")]
fn help_line(help: &Fallback, cx: &App) -> impl IntoElement + use<> {
    let line = div().text_sm().text_color(cx.theme().muted_foreground);
    match help {
        Fallback::Text(text) => line.child(text.clone()),
        Fallback::SetupGuide => {
            const LEAD: &str = "Meanwhile, bind a key in your compositor as the ";
            const LINK: &str = "setup guide";
            let guide = LEAD.len()..LEAD.len() + LINK.len();
            let style = HighlightStyle {
                color: Some(cx.theme().link),
                underline: Some(UnderlineStyle {
                    thickness: px(1.),
                    color: Some(cx.theme().link.opacity(0.5)),
                    wavy: false,
                }),
                ..HighlightStyle::default()
            };
            let text = StyledText::new(format!("{LEAD}{LINK} shows."))
                .with_highlights([(guide.clone(), style)]);
            line.child(
                InteractiveText::new("setup-guide", text)
                    .on_click(vec![guide], |_, _, cx| cx.open_url(SETUP_GUIDE)),
            )
        }
    }
}

fn microphone(settings: &Config, cx: &App) -> SettingGroup {
    let device = item(
        cx,
        "audio.device",
        "Input device",
        "The microphone earwig records from.",
        device_dropdown(settings, cx),
    );
    let error = UiState::mic_error(cx).map(|error| {
        let error = SharedString::from(format!("Microphone error: {error}"));
        SettingItem::render(move |_, _, cx| {
            div()
                .text_sm()
                .text_color(cx.theme().danger)
                .child(error.clone())
        })
    });
    section("Microphone", None, iter::once(device).chain(error))
}

/// System default, every microphone the audio backend lists, by the name
/// people know it by, and the configured device even when it is currently
/// unplugged. A long name ("Family 17h/19h HD Audio Controller Analog
/// Stereo") ends in an ellipsis rather than pushing the dropdown past the
/// window's edge; the open menu shows it whole.
fn device_dropdown(settings: &Config, cx: &App) -> SettingField<SharedString> {
    const MAX_WIDTH: f32 = 240.;
    const DEFAULT: &str = "\u{0}default";
    let mut options: Vec<(SharedString, SharedString)> =
        vec![(DEFAULT.into(), "System default".into())];
    for device in UiState::input_devices(cx) {
        options.push((device.id.clone().into(), device.name.clone().into()));
    }
    if let Some(device) = &settings.audio.device
        && !options.iter().any(|(id, _)| id == device)
    {
        options.push((
            device.clone().into(),
            format!("{device} (not found)").into(),
        ));
    }
    SettingField::dropdown(
        options,
        |cx: &App| {
            config(UiState::handle(cx))
                .audio
                .device
                .map_or(DEFAULT.into(), SharedString::from)
        },
        |v: SharedString, cx: &mut App| {
            let device = (v != DEFAULT).then(|| v.to_string());
            // Another microphone reports its own failure.
            if config(UiState::handle(cx)).audio.device != device {
                UiState::clear_mic_error(cx);
            }
            edit_config(cx, |c| c.audio.device = device);
        },
    )
    .max_w(px(MAX_WIDTH))
}

/// The look of the overlay and of this window. This window follows at once;
/// the overlay takes a change from the next dictation.
fn appearance(pill: &Entity<PillPreview>, cx: &App) -> SettingGroup {
    section(
        "Appearance",
        Some("Overlay changes show from the next dictation."),
        [
            item(
                cx,
                "appearance.theme",
                "Theme",
                "Colours of this window and the overlay.",
                enum_dropdown_with(
                    &[(ThemeSetting::Dark, "Dark"), (ThemeSetting::Light, "Light")],
                    |c| c.appearance.theme,
                    |theme, cx| {
                        edit_config(cx, |c| c.appearance.theme = theme);
                        apply_appearance(cx);
                    },
                ),
            ),
            item(
                cx,
                "overlay.enabled",
                "Show overlay",
                "Live text in a small panel while you speak.",
                SettingField::switch(
                    |cx: &App| config(UiState::handle(cx)).overlay.enabled,
                    |v, cx: &mut App| edit_config(cx, |c| c.overlay.enabled = v),
                ),
            ),
            item(
                cx,
                "overlay.position",
                "Position",
                "Screen edge the overlay sits on.",
                enum_dropdown(
                    &[
                        (OverlayPosition::Bottom, "Bottom"),
                        (OverlayPosition::Top, "Top"),
                    ],
                    |c| c.overlay.position,
                    |c, v| c.overlay.position = v,
                ),
            ),
            item(
                cx,
                "overlay.max_lines",
                "Lines of text",
                "Older lines scroll away.",
                number(
                    1.,
                    12.,
                    1.,
                    |c| u64::from(c.overlay.max_lines),
                    |c, v| c.overlay.max_lines = u32::try_from(v).unwrap_or(u32::MAX),
                ),
            ),
            item(
                cx,
                "overlay.animation",
                "Animation",
                "How the overlay comes and goes.",
                enum_dropdown(
                    &[
                        (OverlayAnimation::Crt, "CRT"),
                        (OverlayAnimation::Instant, "Instant"),
                    ],
                    |c| c.overlay.animation,
                    |c, v| c.overlay.animation = v,
                ),
            ),
            pill_item(
                cx,
                pill,
                "overlay.opacity",
                "Opacity",
                "How solid the overlay is.",
            ),
        ],
    )
}

#[cfg(target_os = "linux")]
fn typing(cx: &App) -> SettingGroup {
    section(
        "Typing",
        None,
        [item(
            cx,
            "insert.method",
            "Insert method",
            "How the final text reaches the focused app.",
            enum_dropdown(
                &[
                    (InsertSetting::Auto, "Automatic"),
                    (InsertSetting::InputMethod, "Input method"),
                    (InsertSetting::Paste, "Clipboard and paste"),
                    (InsertSetting::Portal, "Remote desktop portal"),
                ],
                |c| c.insert.method,
                |c, v| c.insert.method = v,
            ),
        )],
    )
}

/// Starting with the session: the per-user sign-in entry on Windows, a
/// launch agent on macOS.
#[cfg(any(windows, target_os = "macos"))]
fn startup() -> SettingGroup {
    section(
        "Startup",
        None,
        [SettingItem::new(
            if cfg!(windows) {
                "Start at sign-in"
            } else {
                "Start at login"
            },
            SettingField::switch(
                |_: &App| crate::ui::platform::autostart(),
                |enabled, cx: &mut App| {
                    if let Err(e) = crate::ui::platform::set_autostart(enabled) {
                        log::error!("{e:#}");
                    }
                    cx.refresh_windows();
                },
            ),
        )],
    )
}

/// Microphone and Accessibility, each allowed, or with Ask again (the
/// system's prompt) and a way to System Settings.
#[cfg(target_os = "macos")]
fn permissions_group(statuses: &[earwig_core::permissions::PermissionStatus]) -> SettingGroup {
    use earwig_core::permissions::{self, Access};
    use gpui_kit::IntoElement as _;
    use gpui_kit::component::Sizable as _;
    use gpui_kit::component::button::{Button, ButtonVariants as _};
    let items: Vec<SettingItem> = statuses
        .iter()
        .map(|status| {
            let permission = status.permission;
            let granted = status.access == Access::Granted;
            SettingItem::render(move |options, _, cx| {
                let control = if granted {
                    div().text_sm().child("Allowed").into_any_element()
                } else {
                    h_flex()
                        .gap_3()
                        .items_center()
                        .flex_shrink_0()
                        .child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().danger)
                                .child("Not allowed"),
                        )
                        .child(
                            Button::new(SharedString::from(format!("settings-{permission:?}")))
                                .link()
                                .small()
                                .label("System Settings")
                                .on_click(move |_, _, _| permissions::open_settings(permission)),
                        )
                        .child(
                            Button::new(SharedString::from(format!("ask-{permission:?}")))
                                .primary()
                                .small()
                                .label("Ask again")
                                .tooltip(
                                    "After an update, System Settings may still show the old \
                                     earwig ticked; this replaces it.",
                                )
                                .on_click(move |_, _, _| permissions::ask(permission)),
                        )
                        .into_any_element()
                };
                shortcut::row(
                    permission.label(),
                    Some((
                        format!("To {}.", permission.purpose()).into(),
                        shortcut::Tone::Muted,
                    )),
                    control,
                    None,
                    options.layout(),
                    cx,
                )
            })
        })
        .collect();
    section(
        "Permissions",
        Some("macOS lets earwig hear you, see the hotkey and paste only once you allow it."),
        items,
    )
}

/// The Opacity row: the slider where other rows have their field, and the
/// overlay preview below it across the row. Laid out as [`item`]'s rows
/// are, except that the description wraps as narrow as the slider needs:
/// GPUI measures text unwrapped, so a standard row keeps a long description
/// at its widest and pushes a wide field past the window's edge.
fn pill_item(
    cx: &App,
    pill: &Entity<PillPreview>,
    key: &str,
    title: &'static str,
    description: &str,
) -> SettingItem {
    /// The slider's widest beside the title; stacked, it spans the row.
    const SLIDER_WIDTH: f32 = 280.;
    let locked = is_locked(cx, key);
    let description = SharedString::from(hinted(description, locked));
    let keywords = [SharedString::from(title), description.clone()];
    let pill = pill.clone();
    SettingItem::render(move |options, _, cx| {
        let stacked = options.layout() == Axis::Vertical;
        let text = v_flex()
            .map(|this| {
                if stacked {
                    this.w_full()
                } else {
                    this.flex_1().min_w_0().max_w_3_5()
                }
            })
            .child(Label::new(title).text_sm())
            .child(
                div()
                    .size_full()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(description.clone()),
            );
        let slider = PillPreview::slider(&pill, locked, cx);
        let heading = if stacked {
            v_flex().w_full().gap_3().child(text).child(slider.w_full())
        } else {
            h_flex()
                .w_full()
                .justify_between()
                .gap_3()
                .child(text)
                .child(slider.flex_1().max_w(px(SLIDER_WIDTH)))
        };
        v_flex().w_full().gap_3().child(heading).child(pill.clone())
    })
    .keywords(keywords)
    .disabled(locked)
}

/// A dropdown over a config enum, keyed by its serde name.
fn enum_dropdown<T>(
    choices: &[(T, &'static str)],
    get: fn(&Config) -> T,
    set: fn(&mut Config, T),
) -> SettingField<SharedString>
where
    T: Serialize + DeserializeOwned + 'static,
{
    enum_dropdown_with(choices, get, move |value, cx| {
        edit_config(cx, |c| set(c, value));
    })
}

/// [`enum_dropdown`] that applies a choice with `set`.
fn enum_dropdown_with<T>(
    choices: &[(T, &'static str)],
    get: fn(&Config) -> T,
    set: impl Fn(T, &mut App) + 'static,
) -> SettingField<SharedString>
where
    T: Serialize + DeserializeOwned + 'static,
{
    let options = choices
        .iter()
        .map(|(value, label)| (serde_key(value), SharedString::from(*label)))
        .collect();
    SettingField::dropdown(
        options,
        move |cx: &App| serde_key(&get(&config(UiState::handle(cx)))),
        move |key: SharedString, cx: &mut App| match serde_json::from_value::<T>(
            serde_json::Value::String(key.to_string()),
        ) {
            Ok(value) => set(value, cx),
            Err(e) => log::error!("unknown option {key}: {e}"),
        },
    )
}

fn serde_key(value: &impl Serialize) -> SharedString {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(s)) => s.into(),
        _ => SharedString::default(),
    }
}

/// A whole-number field.
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
fn number(
    min: f64,
    max: f64,
    step: f64,
    get: fn(&Config) -> u64,
    set: fn(&mut Config, u64),
) -> SettingField<f64> {
    SettingField::number_input(
        NumberFieldOptions { min, max, step },
        move |cx: &App| get(&config(UiState::handle(cx))) as f64,
        move |v: f64, cx: &mut App| {
            let v = v.clamp(min, max).round() as u64;
            edit_config(cx, |c| set(c, v));
        },
    )
}

fn user_error(cx: &App) -> Option<String> {
    UiState::handle(cx)
        .config()
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .user_error()
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enum_keys_round_trip_through_serde_names() {
        let key = serde_key(&ActivationMode::HoldOrToggle);
        assert_eq!(key, "hold-or-toggle");
        let back: ActivationMode =
            serde_json::from_value(serde_json::Value::String(key.to_string())).unwrap();
        assert_eq!(back, ActivationMode::HoldOrToggle);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn portal_desktops_name_their_settings_app_and_others_get_the_guide() {
        assert_eq!(
            fallback("ubuntu:GNOME", true),
            Fallback::Text(
                "Meanwhile, the shortcut set in Settings → Keyboard → Shortcuts works.".into()
            )
        );
        let Fallback::Text(kde) = fallback("KDE", true) else {
            panic!("KDE's portal shortcut");
        };
        assert!(kde.contains("System Settings"), "{kde}");
        for compositor in ["niri", "sway", "Hyprland", "COSMIC", "GNOME", ""] {
            assert_eq!(
                fallback(compositor, false),
                Fallback::SetupGuide,
                "{compositor}"
            );
        }
    }

    #[test]
    fn the_mode_description_follows_the_mode_and_names_the_key() {
        assert_eq!(
            mode_description(ActivationMode::Hold, Some("Right Alt")),
            "Hold Right Alt while you talk."
        );
        assert_eq!(
            mode_description(ActivationMode::Toggle, Some("Ctrl+Space")),
            "Press Ctrl+Space to start and stop."
        );
        assert_eq!(
            mode_description(ActivationMode::HoldOrToggle, None),
            "Hold or tap the hotkey to talk."
        );
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn macos_shows_keys_with_a_macs_symbols() {
        assert_eq!(
            mode_description(
                ActivationMode::HoldOrToggle,
                Some(&shortcut::label("RightCmd"))
            ),
            "Hold or tap Right ⌘ to talk."
        );
        assert_eq!(shortcut::label("Option+Space"), "⌥ Space");
    }
}
