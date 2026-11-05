//! Recent transcripts, newest first.

use earwig_core::history::HistoryEntry;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::clipboard::Clipboard;
use gpui_kit::component::setting::{SettingGroup, SettingItem, SettingPage};
use gpui_kit::component::{ActiveTheme as _, IconName, Sizable as _, h_flex, v_flex};
use gpui_kit::{App, IntoElement, ParentElement as _, Styled as _, div};

use crate::ui::{UiState, config, now_ms};

pub fn page() -> SettingPage {
    SettingPage::new("History")
        .icon(IconName::Inbox)
        .resettable(false)
        .title_suffix(|_, _| {
            Button::new("clear-history")
                .label("Clear all")
                .icon(IconName::Delete)
                .ghost()
                .small()
                .on_click(|_, _, cx| {
                    if let Err(e) = UiState::handle(cx).history().clear() {
                        log::error!("clearing history: {e}");
                    }
                    cx.refresh_windows();
                })
        })
        .group(SettingGroup::new().item(SettingItem::render(|_, _, cx| entries(cx))))
}

fn entries(cx: &App) -> impl IntoElement + use<> {
    let handle = UiState::handle(cx);
    let entries = handle.history().entries();
    let muted = cx.theme().muted_foreground;
    let empty = if !config(handle).history.enabled {
        Some("History is off. Turn it on under General.")
    } else if entries.is_empty() {
        Some("Nothing yet. Transcripts show up here after you dictate.")
    } else {
        None
    };
    let now = now_ms();
    v_flex()
        .w_full()
        .gap_3()
        .children(empty.map(|text| div().text_sm().text_color(muted).child(text)))
        .children(
            entries
                .into_iter()
                .enumerate()
                .map(|(ix, entry)| row(ix, &entry, now, cx)),
        )
}

fn row(ix: usize, entry: &HistoryEntry, now: u64, cx: &App) -> impl IntoElement + use<> {
    let muted = cx.theme().muted_foreground;
    let status = if entry.method.is_some() {
        String::new()
    } else {
        " · not inserted".to_owned()
    };
    h_flex()
        .w_full()
        .items_start()
        .gap_3()
        .pb_3()
        .border_b_1()
        .border_color(cx.theme().border)
        .child(
            v_flex()
                .flex_1()
                .gap_1()
                .child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child(format!("{}{status}", ago(now, entry.at_ms))),
                )
                .child(div().text_sm().child(entry.text.clone())),
        )
        .child(
            Clipboard::new(("copy-history", ix))
                .tooltip("Copy")
                .value(entry.text.clone()),
        )
}

/// "just now", "5 min ago", "3 h ago", "2 days ago".
fn ago(now_ms: u64, at_ms: u64) -> String {
    let secs = now_ms.saturating_sub(at_ms) / 1000;
    match secs {
        0..60 => "just now".into(),
        60..3600 => format!("{} min ago", secs / 60),
        3600..86_400 => format!("{} h ago", secs / 3600),
        86_400..172_800 => "yesterday".into(),
        _ => format!("{} days ago", secs / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::ago;

    #[test]
    fn ages_round_down_to_the_largest_unit() {
        let now = 10 * 86_400_000;
        assert_eq!(ago(now, now - 59_000), "just now");
        assert_eq!(ago(now, now - 125_000), "2 min ago");
        assert_eq!(ago(now, now - 3 * 3_600_000), "3 h ago");
        assert_eq!(ago(now, now - 86_400_000), "yesterday");
        assert_eq!(ago(now, now - 3 * 86_400_000), "3 days ago");
        assert_eq!(ago(now, now + 5_000), "just now", "clock skew");
    }
}
