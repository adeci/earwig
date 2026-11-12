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
use earwig_core::events::InsertMethod;
use earwig_core::hotkey::trigger::Trigger;
use gpui_kit::component::label::Label;
use gpui_kit::component::setting::{
    NumberFieldOptions, SettingField, SettingGroup, SettingItem, SettingPage,
};
use gpui_kit::component::{ActiveTheme as _, IconName, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, Axis, Entity, HighlightStyle, InteractiveText, IntoElement, ParentElement as _,
    SharedString, Styled as _, StyledText, UnderlineStyle, div, px,
};
use serde::Serialize;
use serde::de::DeserializeOwned;

use super::pill_preview::PillPreview;
use super::{hinted, is_locked, item, section, spaced};
use crate::ui::{UiState, apply_appearance, config, edit_config};

/// Search words for the Hotkey group's rows, whose titles do not say them.
const HOTKEY_KEYWORDS: [&str; 4] = ["hotkey", "shortcut", "key", "bind"];

/// The keys offered on macOS and Windows, which do nothing there on their
/// own, by their config names.
#[cfg(target_os = "macos")]
const KEYS: [(&str, &str); 5] = [
    ("RightCmd", "Right Command"),
    ("RightAlt", "Right Option"),
    ("RightShift", "Right Shift"),
    ("RightCtrl", "Right Control"),
    ("Ctrl+Space", "Control+Space"),
];
#[cfg(not(target_os = "macos"))]
const KEYS: [(&str, &str); 5] = [
    ("RightCtrl", "Right Ctrl"),
    ("RightAlt", "Right Alt"),
    ("RightShift", "Right Shift"),
    ("Ctrl+Space", "Ctrl+Space"),
    ("Ctrl+Alt+Space", "Ctrl+Alt+Space"),
];

/// How to bind the hotkey on each Linux desktop.
const SETUP_GUIDE: &str = "https://github.com/adeci/earwig/blob/main/docs/desktops.md";

pub fn page(pill: &Entity<PillPreview>, settings: &Config, cx: &App) -> SettingPage {
    let mut groups = Vec::new();
    if let Some(error) = user_error(cx) {
        groups.push(section(
            "Settings file",
            None,
            [SettingItem::render(move |_, _, cx| {
                div().text_sm().text_color(cx.theme().danger).child(format!(
                    "Your settings file was ignored because it has an error; \
                     changing a setting here rewrites it.\n{error}"
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
        hotkey(settings, cx),
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

/// The hotkey: the key, where earwig watches one itself (macOS, Windows); what a
/// press does; and a line on where the key is set.
fn hotkey(settings: &Config, cx: &App) -> SettingGroup {
    let key = cfg!(any(windows, target_os = "macos")).then(|| key_label(&settings.hotkey.key));
    let mode = item(
        cx,
        "hotkey.mode",
        "Mode",
        &mode_description(settings.hotkey.mode, key),
        enum_dropdown(
            &[
                (ActivationMode::Hold, "Push to talk"),
                (ActivationMode::Toggle, "Toggle"),
                (ActivationMode::HoldOrToggle, "Both"),
            ],
            |c| c.hotkey.mode,
            |c, v| c.hotkey.mode = v,
        ),
    )
    .keywords(HOTKEY_KEYWORDS);
    let help = hotkey_help(&UiState::caps(cx).desktop, &settings.hotkey.key);
    let help = SettingItem::render(move |_, _, cx| help_line(&help, cx)).keywords(HOTKEY_KEYWORDS);
    #[cfg(any(windows, target_os = "macos"))]
    let items = [
        item(
            cx,
            "hotkey.key",
            "Key",
            "The key you dictate with.",
            key_dropdown(settings),
        )
        .keywords(HOTKEY_KEYWORDS),
        mode,
        help,
    ];
    #[cfg(target_os = "linux")]
    let items = [mode, help];
    section("Hotkey", None, items)
}

/// [`KEYS`], and whatever key is configured besides.
#[cfg(any(windows, target_os = "macos"))]
fn key_dropdown(settings: &Config) -> SettingField<SharedString> {
    let mut options: Vec<(SharedString, SharedString)> = KEYS
        .iter()
        .map(|&(key, label)| (key.into(), label.into()))
        .collect();
    let key = &settings.hotkey.key;
    if !options.iter().any(|(id, _)| id == key) {
        options.push((key.clone().into(), key.clone().into()));
    }
    SettingField::dropdown(
        options,
        |cx: &App| config(UiState::handle(cx)).hotkey.key.into(),
        |key: SharedString, cx: &mut App| edit_config(cx, |c| c.hotkey.key = key.to_string()),
    )
}

/// How the Key row names `key`.
fn key_label(key: &str) -> &str {
    KEYS.iter()
        .find(|(id, _)| *id == key)
        .map_or(key, |&(_, label)| label)
}

/// What a press does in `mode`, naming the key where earwig watches it.
fn mode_description(mode: ActivationMode, key: Option<&str>) -> String {
    let key = key.unwrap_or("the hotkey");
    match mode {
        ActivationMode::Hold => format!("Hold {key} while you talk."),
        ActivationMode::Toggle => format!("Press {key} once to start, and again to stop."),
        ActivationMode::HoldOrToggle => format!("Hold {key} to talk, or tap it to toggle."),
    }
}

/// What the line under Mode says.
#[derive(Debug, PartialEq)]
enum HotkeyHelp {
    Text(SharedString),
    /// "Bind a key in your compositor", linking the setup guide.
    SetupGuide,
}

/// The line under Mode on `desktop`. Windows: what `key`, which earwig
/// watches itself, still does for other apps. GNOME and KDE: where their
/// settings change the shortcut earwig registers through the portal.
/// Anywhere else a key is bound in the compositor's config, as the setup
/// guide shows.
fn hotkey_help(desktop: &str, key: &str) -> HotkeyHelp {
    let desktops: Vec<&str> = desktop.split(':').collect();
    let is = |name: &str| desktops.iter().any(|d| d.eq_ignore_ascii_case(name));
    if is("windows") || is("macos") {
        HotkeyHelp::Text(key_help(key).into())
    } else if is("kde") {
        HotkeyHelp::Text("Change the key in System Settings → Keyboard → Shortcuts.".into())
    } else if is("gnome") {
        HotkeyHelp::Text("Change the key in Settings → Keyboard → Shortcuts.".into())
    } else {
        HotkeyHelp::SetupGuide
    }
}

/// `help` as a line of small text, the setup guide a link.
fn help_line(help: &HotkeyHelp, cx: &App) -> impl IntoElement + use<> {
    let line = div().text_sm().text_color(cx.theme().muted_foreground);
    match help {
        HotkeyHelp::Text(text) => line.child(text.clone()),
        HotkeyHelp::SetupGuide => {
            const LEAD: &str = "Bind a key in your compositor: see the ";
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
            let text =
                StyledText::new(format!("{LEAD}{LINK}.")).with_highlights([(guide.clone(), style)]);
            line.child(
                InteractiveText::new("setup-guide", text)
                    .on_click(vec![guide], |_, _, cx| cx.open_url(SETUP_GUIDE)),
            )
        }
    }
}

/// What `key` does for other apps while earwig watches it: a lone modifier
/// still works with other keys; a key combination no longer reaches them.
fn key_help(key: &str) -> String {
    let name = key_label(key);
    match key.parse::<Trigger>() {
        Ok(trigger) if trigger.held_modifier().is_some() => {
            format!("Pressed together with another key, {name} works as it always does.")
        }
        Ok(_) => format!("Other apps no longer receive {name}."),
        Err(e) => {
            let reason = e.to_string();
            format!(
                "{name} cannot be the hotkey: {}.",
                reason.trim_end_matches('.')
            )
        }
    }
}

fn microphone(settings: &Config, cx: &App) -> SettingGroup {
    let device = item(
        cx,
        "audio.device",
        "Input device",
        &device_description(UiState::default_input_device(cx)),
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

/// The Input device row's description, naming the device the system default
/// is now when the audio backend knows it. The dropdown says only "System
/// default", so a long device name never widens it.
fn device_description(default: Option<&str>) -> String {
    match default {
        Some(name) => {
            format!("The microphone earwig records from; the system default is now {name}.")
        }
        None => "The microphone earwig records from.".to_owned(),
    }
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
                "Colours of this window and the overlay, whatever the desktop uses.",
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
                "How the panel comes and goes: it powers on and off like an old \
                 TV screen, or appears and is gone at once.",
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
                "How solid the overlay is; its text stays sharp.",
            ),
        ],
    )
}

#[cfg(target_os = "linux")]
fn typing(cx: &App) -> SettingGroup {
    let available = UiState::status(cx)
        .insert_methods
        .iter()
        .map(|m| insert_method_label(*m))
        .collect::<Vec<_>>();
    let available = if available.is_empty() {
        "none detected".to_owned()
    } else {
        available.join(", ")
    };
    section(
        "Typing",
        None,
        [item(
            cx,
            "insert.method",
            "Insert method",
            &format!("How the final text reaches the focused app. Available here: {available}."),
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

#[cfg(target_os = "linux")]
fn insert_method_label(method: InsertMethod) -> &'static str {
    match method {
        InsertMethod::InputMethod => "input method",
        InsertMethod::Paste => "paste",
        InsertMethod::PortalPaste => "portal paste",
    }
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

/// Microphone and Accessibility, each granted or with a way to System
/// Settings.
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
            SettingItem::render(move |_, _, cx| {
                h_flex()
                    .w_full()
                    .justify_between()
                    .gap_4()
                    .child(
                        v_flex()
                            .child(div().text_sm().child(permission.label()))
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(format!("To {}.", permission.purpose())),
                            ),
                    )
                    .child(if granted {
                        div().text_sm().child("Allowed").into_any_element()
                    } else {
                        h_flex()
                            .gap_3()
                            .items_center()
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().danger)
                                    .child("Not allowed"),
                            )
                            .child(
                                Button::new(SharedString::from(format!("allow-{permission:?}")))
                                    .primary()
                                    .small()
                                    .label("Open System Settings")
                                    .on_click(move |_, _, _| {
                                        permissions::request(permission);
                                        permissions::open_settings(permission);
                                    }),
                            )
                            .into_any_element()
                    })
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
    fn portal_desktops_name_their_settings_app_and_others_get_the_guide() {
        assert_eq!(
            hotkey_help("ubuntu:GNOME", "RightCtrl"),
            HotkeyHelp::Text("Change the key in Settings → Keyboard → Shortcuts.".into())
        );
        assert_eq!(
            hotkey_help("KDE", "RightCtrl"),
            HotkeyHelp::Text("Change the key in System Settings → Keyboard → Shortcuts.".into())
        );
        for compositor in ["niri", "sway", "Hyprland", "COSMIC", ""] {
            assert_eq!(
                hotkey_help(compositor, "RightCtrl"),
                HotkeyHelp::SetupGuide,
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
            "Press Ctrl+Space once to start, and again to stop."
        );
        assert_eq!(
            mode_description(ActivationMode::HoldOrToggle, None),
            "Hold the hotkey to talk, or tap it to toggle."
        );
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn macos_help_names_the_key_as_a_mac_keyboard_does() {
        let HotkeyHelp::Text(alt) = hotkey_help("macos", "RightAlt") else {
            panic!("macOS points to no setup guide");
        };
        assert!(alt.contains("Right Option"), "{alt}");
        assert_eq!(
            mode_description(ActivationMode::HoldOrToggle, Some(key_label("RightCmd"))),
            "Hold Right Command to talk, or tap it to toggle."
        );
    }

    #[test]
    #[cfg(not(target_os = "macos"))]
    fn windows_help_describes_the_configured_key() {
        let help = |key| match hotkey_help("windows", key) {
            HotkeyHelp::Text(text) => text.to_string(),
            HotkeyHelp::SetupGuide => panic!("Windows points to no setup guide"),
        };
        let alt = help("RightAlt");
        assert!(alt.contains("Right Alt"), "{alt}");
        assert!(!alt.contains("Right Ctrl"), "{alt}");
        assert_eq!(
            help("Ctrl+Space"),
            "Other apps no longer receive Ctrl+Space."
        );
        // Written by hand: named as written.
        let custom = help("LeftAlt");
        assert!(
            custom.contains("LeftAlt works as it always does"),
            "{custom}"
        );
        let broken = help("Ctrl+Hyper");
        assert!(broken.contains("cannot be the hotkey"), "{broken}");
        assert!(!broken.ends_with(".."), "{broken}");
    }

    #[test]
    fn the_system_default_is_named_when_the_backend_knows_it() {
        assert_eq!(
            device_description(Some("Microphone (USB Audio)")),
            "The microphone earwig records from; the system default is now \
             Microphone (USB Audio)."
        );
        assert_eq!(
            device_description(None),
            "The microphone earwig records from."
        );
    }
}
