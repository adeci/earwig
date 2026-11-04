//! General settings: activation, audio, insertion, overlay, history, hotkeys.

use std::sync::PoisonError;

use earwig_core::config::{ActivationMode, Config, InsertSetting, OverlayPosition};
use earwig_core::events::InsertMethod;
use gpui_kit::component::clipboard::Clipboard;
use gpui_kit::component::setting::{
    NumberFieldOptions, SettingField, SettingGroup, SettingItem, SettingPage,
};
use gpui_kit::component::{ActiveTheme as _, IconName, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{App, ParentElement as _, SharedString, Styled as _, div, px};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::ui::{UiState, config, edit_config};

const LOCKED_HINT: &str = "Set by system configuration.";

pub fn page(cx: &App) -> SettingPage {
    let mut groups = Vec::new();
    if let Some(error) = user_error(cx) {
        groups.push(
            SettingGroup::new()
                .title("Settings file")
                .item(SettingItem::render(move |_, _, cx| {
                    div().text_sm().text_color(cx.theme().danger).child(format!(
                        "Your settings file was ignored because it has an error; \
                         changing a setting here rewrites it.\n{error}"
                    ))
                })),
        );
    }
    groups.extend([
        activation(cx),
        audio(cx),
        insertion(cx),
        overlay(cx),
        history(cx),
        hotkeys(),
    ]);
    SettingPage::new("General")
        .icon(IconName::Settings2)
        .default_open(true)
        .resettable(false)
        .groups(groups)
}

fn activation(cx: &App) -> SettingGroup {
    SettingGroup::new().title("Activation").items([
        item(
            cx,
            "hotkey.mode",
            "Mode",
            "Hold or tap: hold to talk, tap to keep recording until the next press.",
            enum_dropdown(
                &[
                    (ActivationMode::HoldOrToggle, "Hold or tap"),
                    (ActivationMode::Hold, "Hold to talk"),
                    (ActivationMode::Toggle, "Press to start and stop"),
                ],
                |c| c.hotkey.mode,
                |c, v| c.hotkey.mode = v,
            ),
        ),
        item(
            cx,
            "hotkey.hold_threshold_ms",
            "Hold threshold (ms)",
            "A press shorter than this counts as a tap.",
            number(
                50.,
                2000.,
                50.,
                |c| c.hotkey.hold_threshold_ms,
                |c, v| c.hotkey.hold_threshold_ms = v,
            ),
        ),
    ])
}

fn audio(cx: &App) -> SettingGroup {
    SettingGroup::new().title("Microphone").items([
        item(
            cx,
            "audio.device",
            "Input device",
            "The microphone earwig records from.",
            device_dropdown(cx),
        ),
        item(
            cx,
            "audio.keep_open",
            "Keep microphone open",
            "Never clips the first word. The desktop's microphone indicator stays on.",
            SettingField::switch(
                |cx: &App| config(UiState::handle(cx)).audio.keep_open,
                |v, cx: &mut App| edit_config(cx, |c| c.audio.keep_open = v),
            ),
        ),
        item(
            cx,
            "audio.preroll_ms",
            "Pre-roll (ms)",
            "Audio from just before the press included in each recording.",
            number(
                0.,
                2000.,
                50.,
                |c| c.audio.preroll_ms,
                |c, v| c.audio.preroll_ms = v,
            ),
        ),
    ])
}

/// System default, every microphone the audio backend lists, and the
/// configured device even when it is currently unplugged.
fn device_dropdown(cx: &App) -> SettingField<SharedString> {
    const DEFAULT: &str = "\u{0}default";
    let mut options: Vec<(SharedString, SharedString)> =
        vec![(DEFAULT.into(), "System default".into())];
    for device in UiState::input_devices(cx) {
        options.push((device.clone().into(), device.clone().into()));
    }
    if let Some(device) = config(UiState::handle(cx)).audio.device
        && !options.iter().any(|(id, _)| id.as_ref() == device)
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
            edit_config(cx, |c| c.audio.device = device);
        },
    )
}

fn insertion(cx: &App) -> SettingGroup {
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
    SettingGroup::new().title("Typing").item(item(
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
    ))
}

fn insert_method_label(method: InsertMethod) -> &'static str {
    match method {
        InsertMethod::InputMethod => "input method",
        InsertMethod::Paste => "paste",
        InsertMethod::PortalPaste => "portal paste",
        InsertMethod::PortalType => "portal typing",
    }
}

fn overlay(cx: &App) -> SettingGroup {
    SettingGroup::new().title("Overlay").items([
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
    ])
}

fn history(cx: &App) -> SettingGroup {
    SettingGroup::new().title("History").items([
        item(
            cx,
            "history.enabled",
            "Keep history",
            "Recent transcripts are stored on this computer only.",
            SettingField::switch(
                |cx: &App| config(UiState::handle(cx)).history.enabled,
                |v, cx: &mut App| edit_config(cx, |c| c.history.enabled = v),
            ),
        ),
        item(
            cx,
            "history.limit",
            "Entries kept",
            "Older transcripts are dropped.",
            number(
                1.,
                1000.,
                10.,
                |c| c.history.limit as u64,
                |c, v| c.history.limit = usize::try_from(v).unwrap_or(usize::MAX),
            ),
        ),
    ])
}

/// How the hotkey is bound on this desktop. earwig cannot bind keys itself
/// on Wayland; the compositor (or the portal) sends it press and release.
fn hotkeys() -> SettingGroup {
    SettingGroup::new()
        .title("Hotkey")
        .item(SettingItem::render(|_, _, cx| {
            let desktop = UiState::caps(cx).desktop.clone();
            let sources = UiState::status(cx).hotkey_sources.join(", ");
            let trigger = config(UiState::handle(cx)).hotkey.portal_trigger;
            let muted = cx.theme().muted_foreground;
            let (intro, snippet) = hotkey_help(&desktop, &trigger);
            v_flex()
                .w_full()
                .gap_2()
                .text_sm()
                .child(intro)
                .when_some(snippet, |this, snippet| {
                    this.child(
                        h_flex()
                            .w_full()
                            .items_start()
                            .gap_2()
                            .p_2()
                            .rounded(px(6.))
                            .bg(cx.theme().muted)
                            .child(
                                div()
                                    .flex_1()
                                    .font_family(cx.theme().mono_font_family.clone())
                                    .text_xs()
                                    .child(snippet.clone()),
                            )
                            .child(
                                Clipboard::new("copy-hotkey-snippet")
                                    .tooltip("Copy")
                                    .value(snippet),
                            ),
                    )
                })
                .when(!sources.is_empty(), |this| {
                    this.child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child(format!("Listening on: {sources}")),
                    )
                })
        }))
}

fn hotkey_help(desktop: &str, trigger: &str) -> (String, Option<String>) {
    let desktops: Vec<&str> = desktop.split(':').collect();
    let is = |name: &str| desktops.iter().any(|d| d.eq_ignore_ascii_case(name));
    if is("niri") {
        (
            "Add this to the binds section of ~/.config/niri/config.kdl:".into(),
            Some(
                "Mod+Space { press { spawn \"earwig\" \"press\"; } \
                 release { spawn \"earwig\" \"release\"; } }"
                    .into(),
            ),
        )
    } else if is("sway") {
        (
            "Add this to your sway config:".into(),
            Some(
                "bindsym --no-repeat $mod+space exec earwig press\n\
                 bindsym --release $mod+space exec earwig release"
                    .into(),
            ),
        )
    } else if is("hyprland") {
        (
            "Add this to hyprland.conf:".into(),
            Some(
                "bind = SUPER, space, exec, earwig press\n\
                 bindr = SUPER, space, exec, earwig release"
                    .into(),
            ),
        )
    } else if is("gnome") || is("kde") {
        (
            format!(
                "earwig registers a global shortcut through the desktop portal \
                 (suggested: {trigger}). Change it in System Settings under \
                 Keyboard → Shortcuts."
            ),
            None,
        )
    } else {
        (
            "Bind a key in your desktop to these commands. Without release \
             events, bind only the toggle command."
                .into(),
            Some(
                "on press:   earwig press\non release: earwig release\nor:         earwig toggle"
                    .into(),
            ),
        )
    }
}

/// A setting row; locked keys render disabled with a hint.
fn item<T: 'static>(
    cx: &App,
    key: &str,
    title: &'static str,
    description: &str,
    field: SettingField<T>,
) -> SettingItem
where
    SettingField<T>: gpui_kit::component::setting::AnySettingField,
{
    let locked = UiState::handle(cx)
        .config()
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .is_locked(key);
    let description = if locked {
        format!("{description} {LOCKED_HINT}")
    } else {
        description.to_owned()
    };
    SettingItem::new(title, field)
        .description(description)
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
            Ok(value) => edit_config(cx, |c| set(c, value)),
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
    fn desktop_lists_pick_the_matching_binding_help() {
        assert!(
            hotkey_help("niri", "LOGO+space")
                .1
                .unwrap()
                .contains("release")
        );
        assert!(
            hotkey_help("ubuntu:GNOME", "LOGO+space")
                .0
                .contains("LOGO+space")
        );
        assert!(hotkey_help("Hyprland", "").1.unwrap().starts_with("bind ="));
    }
}
