//! The Shortcut row: the key earwig watches, recorded by pressing it or
//! reset to this system's default, and on Linux the row that lets earwig
//! read the keyboard.

use earwig_core::config::HotkeyConfig;
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
use crate::ui::edit_config;

/// A recording in progress, and what the last one left to say.
#[derive(Default)]
pub struct ShortcutField {
    recording: bool,
    /// The modifiers held now, waiting for a key, as the chip shows them.
    holding: Option<SharedString>,
    note: Option<Note>,
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
    fn record(&mut self, cx: &mut Context<Self>) {
        let (tx, rx) = flume::unbounded();
        hotkey::record(move |step| {
            let _ = tx.send(step);
        });
        self.recording = true;
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
        self.recording = false;
        cx.notify();
    }

    /// Back to this system's default key, which leaves `[hotkey] key` out
    /// of the user's file, as any setting set back to its default does.
    fn reset(&mut self, cx: &mut Context<Self>) {
        if self.recording {
            hotkey::stop_recording();
            self.recording = false;
            self.holding = None;
        }
        self.note = None;
        edit_config(cx, |c| c.hotkey.key = HotkeyConfig::DEFAULT_KEY.into());
        cx.notify();
    }

    /// One step of the recording; true once it is over.
    fn step(&mut self, step: Recorded, cx: &mut Context<Self>) -> bool {
        match step {
            Recorded::Holding(label) => {
                self.holding = Some(label.into());
                cx.notify();
                return false;
            }
            Recorded::Cancelled => {}
            Recorded::Done(hotkey) => {
                self.note = match hotkey.check(&hotkey::desktop()) {
                    Check::Reject(reason) => Some(Note::Refused(reason.into())),
                    check => {
                        let key = hotkey.to_string();
                        edit_config(cx, |c| c.hotkey.key = key);
                        match check {
                            Check::Warn(reason) => Some(Note::Warning(reason.into())),
                            _ => None,
                        }
                    }
                };
            }
        }
        self.recording = false;
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
        if self.recording {
            hotkey::stop_recording();
        }
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

/// `key` as the row shows it: its label, or as written when it does not
/// parse.
pub(super) fn label(key: &str) -> String {
    if key.trim().is_empty() {
        return "None".into();
    }
    key.parse::<Hotkey>()
        .map_or_else(|_| key.to_owned(), |hotkey| hotkey.label())
}

/// Whether `key` is this system's default, however it is written.
fn is_default(key: &str) -> bool {
    key.parse::<Hotkey>().ok() == HotkeyConfig::DEFAULT_KEY.parse().ok()
}

/// The Shortcut row for `configured` (`[hotkey] key`) as it is watched now.
pub(super) fn item(field: &Entity<ShortcutField>, configured: &str, cx: &App) -> SettingItem {
    let locked = is_locked(cx, "hotkey.key");
    // Another key than the default, which the reset button brings back.
    let resettable = !locked && !is_default(configured);
    let reset_tip = SharedString::from(format!("Reset to {}", label(HotkeyConfig::DEFAULT_KEY)));
    let label = label(configured);
    let configured = configured.to_owned();
    let field = field.clone();
    SettingItem::render(move |options, _, cx| {
        let status = hotkey::status();
        let state = field.read(cx);
        // A locked row with nothing else to say says that it is.
        let line = line(state, &status, &configured, &label)
            .map(|(line, tone)| (hinted(&line, locked), tone))
            .or_else(|| locked.then(|| (LOCKED_HINT.to_owned(), Tone::Muted)));
        let note = note(state);
        let can_record = status.keyboard == KeyboardState::Watching && !locked;
        let shown: SharedString = if state.recording {
            state
                .holding
                .clone()
                .unwrap_or_else(|| "Press keys…".into())
        } else {
            label.clone().into()
        };
        let button = if state.recording {
            Button::new("cancel-shortcut")
                .label("Cancel")
                .ghost()
                .small()
                .on_click({
                    let field = field.clone();
                    move |_, _, cx| field.update(cx, ShortcutField::cancel)
                })
        } else {
            Button::new("record-shortcut")
                .label("Record")
                .outline()
                .small()
                .disabled(!can_record)
                .on_click({
                    let field = field.clone();
                    move |_, _, cx| field.update(cx, ShortcutField::record)
                })
        };
        let reset = resettable.then(|| {
            Button::new("reset-shortcut")
                .icon(IconName::RotateCcw)
                .ghost()
                .small()
                .tooltip(reset_tip.clone())
                .on_click({
                    let field = field.clone();
                    move |_, _, cx| field.update(cx, ShortcutField::reset)
                })
        });
        let keycap = div()
            .px_2()
            .py_0p5()
            .min_w(px(96.))
            .rounded(cx.theme().radius)
            .border_1()
            .border_color(if state.recording {
                cx.theme().primary
            } else {
                cx.theme().border
            })
            .text_sm()
            .when(state.recording, |this| {
                this.text_color(cx.theme().muted_foreground)
            })
            .child(shown);
        let control = h_flex()
            .gap_2()
            .items_center()
            .flex_shrink_0()
            .child(keycap)
            .children(reset)
            .child(button);
        row(
            "Shortcut",
            line.map(|(line, tone)| (line.into(), tone)),
            control.into_any_element(),
            note.map(|(note, tone)| (note.into(), tone)),
            options.layout(),
            cx,
        )
    })
    .disabled(locked)
}

/// The line under the Shortcut title: what recording asks for, or what
/// `configured` does now.
fn line(
    state: &ShortcutField,
    status: &HotkeyStatus,
    configured: &str,
    label: &str,
) -> Option<(String, Tone)> {
    if state.recording {
        return Some(("Press the keys you want.".into(), Tone::Muted));
    }
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

/// What the last recording left to say, under the whole row: a reason
/// takes more room than the line beside the controls has.
fn note(state: &ShortcutField) -> Option<(String, Tone)> {
    match state.note.as_ref().filter(|_| !state.recording)? {
        Note::Refused(reason) => Some((format!("Not taken: {reason}"), Tone::Danger)),
        Note::Warning(reason) => Some((reason.to_string(), Tone::Warning)),
    }
}

/// What the watched key still does for other apps: modifiers alone work as
/// always together with other keys; a key with modifiers no longer reaches
/// them. Nothing where earwig cannot keep it from them: that does no harm,
/// and `earwig status` says why.
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

/// A row laid out as gpui-kit's own: title and maybe a line under it, and
/// `control` beside them, or below when the window is narrow.
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

/// Linux without keyboard access: how to give it.
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
    }

    #[test]
    fn why_other_apps_still_get_the_key_is_left_to_earwig_status() {
        let status = status("Ctrl+Space", false, Some("keyd holds the keyboard"));
        assert_eq!(what_it_does(&status, "Ctrl+Space"), None);
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
    fn a_refused_key_is_explained_under_the_row_until_the_next_recording() {
        let mut state = ShortcutField::default();
        state.note = Some(Note::Refused(
            "A types a character; add Ctrl, Alt or another modifier.".into(),
        ));
        assert_eq!(
            note(&state),
            Some((
                "Not taken: A types a character; add Ctrl, Alt or another modifier.".into(),
                Tone::Danger
            ))
        );
        state.recording = true;
        assert_eq!(note(&state), None);
    }

    #[test]
    fn keys_show_by_their_labels_or_as_written() {
        assert_eq!(label(""), "None");
        assert_eq!(label("Ctrl+Hyper"), "Ctrl+Hyper");
        assert_eq!(label("pageup"), "Page Up");
    }
}
