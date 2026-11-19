//! The Shortcut and Discard rows (record a key, or reset to the default) and, on Linux,
//! keyboard access.

use earwig_core::config::{Config, HotkeyConfig};
use earwig_core::events::{HotkeyStatus, KeyboardState};
use earwig_core::hotkey::{self, Check, Hotkey, Recorded};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::label::Label;
use gpui_kit::component::setting::SettingItem;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Axis, Context, Entity, Hsla, IntoElement as _, ParentElement as _,
    SharedString, Styled as _, div, px,
};

use super::{LOCKED_HINT, hinted, is_locked};
use crate::ui::{UiState, config, edit_config};

/// Which shortcut a row sets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Target {
    Dictate,
    Discard,
}

/// A recording in progress, and what the last one left to say.
#[derive(Default)]
pub struct ShortcutField {
    recording: Option<Target>,
    /// Modifiers held so far, as the keycap shows them.
    holding: Option<SharedString>,
    note: Option<(Target, Note)>,
    /// Linux: the keyboard access helper.
    #[cfg(target_os = "linux")]
    allowing: Allowing,
}

#[derive(Clone)]
enum Note {
    /// The recorded key was not taken, for this reason.
    Refused(SharedString),
    /// It was taken, with this catch.
    Warning(SharedString),
}

#[cfg(target_os = "linux")]
#[derive(Clone, Default)]
enum Allowing {
    #[default]
    Idle,
    /// pkexec is asking for the password.
    Asking,
    Failed(SharedString),
}

impl ShortcutField {
    fn record(&mut self, target: Target, cx: &mut Context<Self>) {
        let (tx, rx) = flume::unbounded();
        hotkey::record(move |step| {
            let _ = tx.send(step);
        });
        self.recording = Some(target);
        self.holding = None;
        self.note = None;
        cx.spawn(async move |this, cx| {
            while let Ok(step) = rx.recv_async().await {
                let done = this.update(cx, |field, cx| field.step(step, cx));
                if done.unwrap_or(true) {
                    break;
                }
            }
        })
        .detach();
        cx.notify();
    }

    fn cancel(&mut self, cx: &mut Context<Self>) {
        hotkey::stop_recording();
        self.recording = None;
        cx.notify();
    }

    /// Set `target` to `key`; `None`, its default, drops it from the user's file.
    fn save(&mut self, target: Target, key: Option<String>, cx: &mut Context<Self>) {
        if self.recording.take().is_some() {
            hotkey::stop_recording();
            self.holding = None;
        }
        self.note = None;
        edit_config(cx, |c| set(c, target, key));
        cx.notify();
    }

    /// One step of the recording; true once it is over.
    fn step(&mut self, step: Recorded, cx: &mut Context<Self>) -> bool {
        let Some(target) = self.recording else {
            return true;
        };
        match step {
            Recorded::Holding(label) => {
                self.holding = Some(label.into());
                cx.notify();
                return false;
            }
            Recorded::Cancelled => {}
            Recorded::Done(hotkey) => {
                let key = hotkey.to_string();
                let check = match target {
                    Target::Dictate => hotkey.check(&hotkey::desktop()),
                    Target::Discard => {
                        let mut settings = config(UiState::handle(cx)).hotkey;
                        settings.discard = Some(key.clone());
                        hotkey::discard(&settings)
                            .map_or_else(|e| Check::Reject(e.to_string()), |_| Check::Fine)
                    }
                };
                let note = match check {
                    Check::Reject(reason) => Some(Note::Refused(reason.into())),
                    check => {
                        edit_config(cx, |c| set(c, target, Some(key)));
                        match check {
                            Check::Warn(reason) => Some(Note::Warning(reason.into())),
                            _ => None,
                        }
                    }
                };
                self.note = note.map(|note| (target, note));
            }
        }
        self.recording = None;
        self.holding = None;
        cx.notify();
        true
    }

    /// Install the udev rule through pkexec, off the main thread.
    #[cfg(target_os = "linux")]
    fn allow(&mut self, cx: &mut Context<Self>) {
        let (tx, rx) = flume::bounded(1);
        let spawned = std::thread::Builder::new()
            .name("earwig-keyboard-access".into())
            .spawn(move || {
                let _ = tx.send(hotkey::linux::allow_keyboard_access());
            });
        if let Err(e) = spawned {
            self.allowing = Allowing::Failed(e.to_string().into());
            cx.notify();
            return;
        }
        self.allowing = Allowing::Asking;
        cx.spawn(async move |this, cx| {
            let result = rx.recv_async().await;
            let _ = this.update(cx, |field, cx| {
                field.allowing = match result {
                    Ok(Ok(())) => Allowing::Idle,
                    Ok(Err(e)) => Allowing::Failed(format!("{e:#}").into()),
                    Err(_) => Allowing::Failed("the helper stopped".into()),
                };
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}

impl Drop for ShortcutField {
    fn drop(&mut self) {
        if self.recording.is_some() {
            hotkey::stop_recording();
        }
    }
}

/// Set `target` to `key`; `None` is its default.
fn set(config: &mut Config, target: Target, key: Option<String>) {
    match target {
        Target::Dictate => {
            config.hotkey.key = key.unwrap_or_else(|| HotkeyConfig::DEFAULT_KEY.into());
        }
        Target::Discard => config.hotkey.discard = key,
    }
}

/// How the line under a row's title reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Tone {
    Muted,
    Warning,
    Danger,
}

impl Tone {
    fn color(self, cx: &App) -> Hsla {
        match self {
            Self::Muted => cx.theme().muted_foreground,
            Self::Warning => cx.theme().warning,
            Self::Danger => cx.theme().danger,
        }
    }
}

/// `key` as the row shows it, or as written if it doesn't parse.
pub(super) fn label(key: &str) -> String {
    if key.trim().is_empty() {
        return "None".into();
    }
    key.parse::<Hotkey>()
        .map_or_else(|_| key.to_owned(), |hotkey| hotkey.label())
}

/// Whether `key` is the default, however it's written.
fn is_default(key: &str) -> bool {
    key.parse::<Hotkey>().ok() == HotkeyConfig::DEFAULT_KEY.parse().ok()
}

/// The discard shortcut as the row shows it, or as written if it is not taken.
fn discard_label(settings: &HotkeyConfig, discard: &Result<Option<Hotkey>, String>) -> String {
    match discard {
        Ok(discard) => discard
            .as_ref()
            .map_or_else(|| "None".into(), Hotkey::label),
        Err(_) => label(settings.discard.as_deref().unwrap_or_default()),
    }
}

/// The row's title, its config key, its shortcut as it shows, the default's, and whether it is
/// set apart from the default.
fn parts(
    target: Target,
    settings: &HotkeyConfig,
    discard: &Result<Option<Hotkey>, String>,
) -> (&'static str, &'static str, String, String, bool) {
    match target {
        Target::Dictate => (
            "Shortcut",
            "hotkey.key",
            label(&settings.key),
            label(HotkeyConfig::DEFAULT_KEY),
            !is_default(&settings.key),
        ),
        Target::Discard => {
            let unset = HotkeyConfig {
                discard: None,
                ..settings.clone()
            };
            let default = hotkey::discard(&unset).map_err(|e| e.to_string());
            (
                "Discard",
                "hotkey.discard",
                discard_label(settings, discard),
                discard_label(&unset, &default),
                settings.discard.is_some(),
            )
        }
    }
}

/// An icon button that sets `target` to `key` (`None`: its default).
fn save_button(
    field: &Entity<ShortcutField>,
    target: Target,
    icon: IconName,
    tip: SharedString,
    key: Option<String>,
) -> Button {
    let field = field.clone();
    Button::new(SharedString::from(format!("{tip}-{target:?}")))
        .icon(icon)
        .ghost()
        .small()
        .tooltip(tip)
        .on_click(move |_, _, cx| {
            field.update(cx, |field, cx| field.save(target, key.clone(), cx));
        })
}

pub(super) fn item(
    field: &Entity<ShortcutField>,
    target: Target,
    settings: &HotkeyConfig,
    cx: &App,
) -> SettingItem {
    let discard = hotkey::discard(settings).map_err(|e| e.to_string());
    let (title, config_key, label, default, changed) = parts(target, settings, &discard);
    let locked = is_locked(cx, config_key);
    let resettable = !locked && changed;
    let clearable = !locked && target == Target::Discard && !matches!(discard, Ok(None));
    let reset_tip = SharedString::from(format!("Reset to {default}"));
    let key = settings.key.clone();
    let field = field.clone();
    SettingItem::render(move |options, _, cx| {
        let status = hotkey::status();
        let state = field.read(cx);
        let recording = state.recording == Some(target);
        let line = if recording {
            Some(("Press the keys you want.".to_owned(), Tone::Muted))
        } else if target == Target::Dictate {
            line(&status, &key, &label)
        } else {
            Some(discard_line(&discard, &label))
        };
        // A locked row with nothing else to say says it's locked.
        let line = line
            .map(|(line, tone)| (hinted(&line, locked), tone))
            .or_else(|| locked.then(|| (LOCKED_HINT.to_owned(), Tone::Muted)));
        let note = note(state, target);
        let can_record = status.keyboard == KeyboardState::Watching && !locked;
        let shown: SharedString = if recording {
            state
                .holding
                .clone()
                .unwrap_or_else(|| "Press keys…".into())
        } else {
            label.clone().into()
        };
        let button = if recording {
            Button::new(SharedString::from(format!("cancel-{title}")))
                .label("Cancel")
                .ghost()
                .small()
                .on_click({
                    let field = field.clone();
                    move |_, _, cx| field.update(cx, ShortcutField::cancel)
                })
        } else {
            Button::new(SharedString::from(format!("record-{title}")))
                .label("Record")
                .outline()
                .small()
                .disabled(!can_record)
                .on_click({
                    let field = field.clone();
                    move |_, _, cx| field.update(cx, |field, cx| field.record(target, cx))
                })
        };
        let reset = resettable
            .then(|| save_button(&field, target, IconName::RotateCcw, reset_tip.clone(), None));
        let clear = clearable.then(|| {
            save_button(
                &field,
                target,
                IconName::Close,
                "Clear".into(),
                Some(String::new()),
            )
        });
        let keycap = div()
            .px_2()
            .py_0p5()
            .min_w(px(96.))
            .rounded(cx.theme().radius)
            .border_1()
            .border_color(if recording {
                cx.theme().primary
            } else {
                cx.theme().border
            })
            .text_sm()
            .when(recording, |this| {
                this.text_color(cx.theme().muted_foreground)
            })
            .child(shown);
        let control = h_flex()
            .gap_2()
            .items_center()
            .flex_shrink_0()
            .child(keycap)
            .children(reset)
            .children(clear)
            .child(button);
        row(
            title,
            line.map(|(line, tone)| (line.into(), tone)),
            control.into_any_element(),
            note.map(|(note, tone)| (note.into(), tone)),
            options.layout(),
            cx,
        )
    })
    .disabled(locked)
}

fn line(status: &HotkeyStatus, configured: &str, label: &str) -> Option<(String, Tone)> {
    if configured.trim().is_empty() {
        return Some(("earwig watches no key.".into(), Tone::Muted));
    }
    if status.key.is_empty() {
        let reason = status
            .reason
            .as_deref()
            .unwrap_or("it cannot be the shortcut");
        return Some((format!("{label} is not watched: {reason}"), Tone::Danger));
    }
    let line = match status.keyboard {
        KeyboardState::NoAccess if cfg!(target_os = "macos") => {
            "Allow Accessibility in Permissions first."
        }
        KeyboardState::NoAccess => "earwig may not read the keyboard yet.",
        KeyboardState::NoKeyboard => "No keyboard is plugged in.",
        KeyboardState::Stopped => "earwig is not watching the keyboard.",
        KeyboardState::Watching => return what_it_does(status, label),
    };
    Some((line.into(), Tone::Muted))
}

/// What the Discard row does, or why its shortcut is not taken.
fn discard_line(discard: &Result<Option<Hotkey>, String>, label: &str) -> (String, Tone) {
    match discard {
        Ok(_) => ("Throws away the dictation in progress.".into(), Tone::Muted),
        Err(reason) => (format!("{label} is not watched: {reason}"), Tone::Danger),
    }
}

/// Under the whole row: a reason needs more room than the line beside the controls.
fn note(state: &ShortcutField, target: Target) -> Option<(String, Tone)> {
    match state.note.as_ref().filter(|(row, _)| *row == target)? {
        (_, Note::Refused(reason)) => Some((format!("Not taken: {reason}"), Tone::Danger)),
        (_, Note::Warning(reason)) => Some((reason.to_string(), Tone::Warning)),
    }
}

/// What the key still does for other apps; nothing if earwig can't block it (`earwig status`).
fn what_it_does(status: &HotkeyStatus, label: &str) -> Option<(String, Tone)> {
    let modifiers_alone = status
        .key
        .parse::<Hotkey>()
        .is_ok_and(|h| h.is_modifier_only());
    if modifiers_alone {
        Some((
            format!("Pressed with another key, {label} works as usual."),
            Tone::Muted,
        ))
    } else {
        status.blocking.then(|| {
            (
                format!("Other apps no longer receive {label}."),
                Tone::Muted,
            )
        })
    }
}

/// Laid out like gpui-kit's rows: `control` beside the title, or below it when narrow.
pub(super) fn row(
    title: &'static str,
    line: Option<(SharedString, Tone)>,
    control: AnyElement,
    note: Option<(SharedString, Tone)>,
    layout: Axis,
    cx: &App,
) -> AnyElement {
    let stacked = layout == Axis::Vertical;
    let text = v_flex()
        .map(|this| {
            if stacked {
                this.w_full()
            } else {
                this.flex_1().min_w_0()
            }
        })
        .child(Label::new(title).text_sm())
        .when_some(line, |this, (line, tone)| {
            this.child(div().text_sm().text_color(tone.color(cx)).child(line))
        });
    let row = if stacked {
        v_flex().w_full().gap_2().child(text).child(control)
    } else {
        h_flex()
            .w_full()
            .justify_between()
            .gap_3()
            .child(text)
            .child(control)
    };
    match note {
        Some((note, tone)) => v_flex()
            .w_full()
            .gap_1()
            .child(row)
            .child(div().text_sm().text_color(tone.color(cx)).child(note))
            .into_any_element(),
        None => row.into_any_element(),
    }
}

#[cfg(target_os = "linux")]
pub(super) fn access_item(field: &Entity<ShortcutField>) -> SettingItem {
    use earwig_core::hotkey::linux::{AccessFix, access_fix};
    let field = field.clone();
    SettingItem::render(move |options, _, cx| {
        if access_fix() == AccessFix::NixosModule {
            return row(
                "Keyboard access",
                Some((
                    "Enable services.earwig in your NixOS configuration to let earwig read the \
                     keyboard."
                        .into(),
                    Tone::Muted,
                )),
                div().into_any_element(),
                None,
                options.layout(),
                cx,
            );
        }
        let allowing = field.read(cx).allowing.clone();
        let line = match &allowing {
            Allowing::Idle => ("Let earwig read the keyboard.".into(), Tone::Muted),
            Allowing::Asking => ("Waiting for the password…".into(), Tone::Muted),
            Allowing::Failed(e) => (format!("Keyboard access: {e}").into(), Tone::Danger),
        };
        let button = Button::new("allow-keyboard")
            .label("Allow")
            .primary()
            .small()
            .disabled(matches!(allowing, Allowing::Asking))
            .on_click({
                let field = field.clone();
                move |_, _, cx| field.update(cx, ShortcutField::allow)
            });
        row(
            "Keyboard access",
            Some(line),
            button.into_any_element(),
            None,
            options.layout(),
            cx,
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(key: &str, blocking: bool, reason: Option<&str>) -> HotkeyStatus {
        HotkeyStatus {
            source: "keyboard".into(),
            key: key.into(),
            discard: String::new(),
            keyboard: KeyboardState::Watching,
            blocking,
            reason: reason.map(Into::into),
        }
    }

    #[test]
    fn the_line_says_what_the_key_still_does_for_other_apps() {
        assert_eq!(
            what_it_does(&status("RightCtrl", false, None), "Right Ctrl"),
            Some((
                "Pressed with another key, Right Ctrl works as usual.".into(),
                Tone::Muted
            ))
        );
        assert_eq!(
            what_it_does(&status("Ctrl+Space", true, None), "Ctrl+Space"),
            Some((
                "Other apps no longer receive Ctrl+Space.".into(),
                Tone::Muted
            ))
        );
        // Why it can't is left to `earwig status`.
        let unblocked = status("Ctrl+Space", false, Some("keyd holds the keyboard"));
        assert_eq!(what_it_does(&unblocked, "Ctrl+Space"), None);
    }

    #[test]
    fn only_another_key_than_the_default_can_be_reset() {
        assert!(is_default(HotkeyConfig::DEFAULT_KEY));
        assert!(is_default(&HotkeyConfig::DEFAULT_KEY.to_lowercase()));
        assert!(!is_default("RightCtrl"));
        assert!(!is_default(""));
        assert!(!is_default("Ctrl+Hyper"));
    }

    #[test]
    fn keys_show_by_their_labels_or_as_written() {
        assert_eq!(label(""), "None");
        assert_eq!(label("Ctrl+Hyper"), "Ctrl+Hyper");
        assert_eq!(label("pageup"), "Page Up");
    }

    #[test]
    fn the_discard_row_shows_none_or_a_shortcut_not_taken_as_written() {
        let row = |discard: Option<&str>| {
            let settings = HotkeyConfig {
                key: "Ctrl+Shift+F9".into(),
                discard: discard.map(Into::into),
                ..HotkeyConfig::default()
            };
            let discard = hotkey::discard(&settings).map_err(|e| e.to_string());
            let label = discard_label(&settings, &discard);
            let (_, tone) = discard_line(&discard, &label);
            (label, tone)
        };
        assert_eq!(
            row(None),
            ("None".into(), Tone::Muted),
            "Shift is in the key"
        );
        assert_eq!(row(Some("Ctrl+Hyper")), ("Ctrl+Hyper".into(), Tone::Danger));
    }
}
