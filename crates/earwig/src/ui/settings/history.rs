//! Recent transcripts, newest first, below whether and how many are kept.

use earwig_core::config::Config;
use earwig_core::history::HistoryEntry;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::clipboard::Clipboard;
use gpui_kit::component::setting::{SettingField, SettingGroup, SettingItem, SettingPage};
use gpui_kit::component::{ActiveTheme as _, IconName, Sizable as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, App, ClickEvent, Entity, IntoElement, ParentElement as _, SharedString,
    Styled as _, Window, div,
};

use super::{item, spaced};
use crate::ui::{UiState, config, edit_config, now_ms};

/// The choices for Entries kept.
const LIMITS: [usize; 3] = [50, 200, 1000];

/// What the History page remembers between renders.
#[derive(Debug, Default)]
pub struct HistoryPage {
    /// Clear all was clicked: Cancel or Delete answers.
    asking: bool,
}

impl HistoryPage {
    /// Clear all: ask before deleting anything.
    fn ask(&mut self) {
        self.asking = true;
    }

    /// Cancel: keep everything.
    fn keep(&mut self) {
        self.asking = false;
    }

    /// Delete: whether to delete every transcript now, which only an asked
    /// question allows.
    fn confirm(&mut self) -> bool {
        std::mem::take(&mut self.asking)
    }
}

pub fn page(view: &Entity<HistoryPage>, settings: &Config, cx: &App) -> SettingPage {
    let view = view.clone();
    SettingPage::new("History")
        .icon(IconName::Inbox)
        .resettable(false)
        .title_suffix(move |_, cx| clear_all(&view, cx))
        .groups(spaced(vec![
            SettingGroup::new().items([
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
                    SettingField::dropdown(
                        limit_options(settings.history.limit),
                        |cx: &App| config(UiState::handle(cx)).history.limit.to_string().into(),
                        |choice: SharedString, cx: &mut App| match choice.parse() {
                            Ok(limit) => edit_config(cx, |c| c.history.limit = limit),
                            Err(e) => log::error!("entries kept {choice}: {e}"),
                        },
                    ),
                ),
            ]),
            SettingGroup::new().item(
                SettingItem::render(|_, _, cx| entries(cx)).keywords(["history", "transcript"]),
            ),
        ]))
}

/// Clear all beside the title; once clicked, its question in its place.
fn clear_all(view: &Entity<HistoryPage>, cx: &App) -> AnyElement {
    let count = UiState::handle(cx).history().len();
    if count == 0 {
        return div().into_any_element();
    }
    let answer = |action: fn(&mut HistoryPage)| {
        let view = view.clone();
        move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
            view.update(cx, |page, cx| {
                action(page);
                cx.notify();
            });
        }
    };
    if !view.read(cx).asking {
        return Button::new("clear-history")
            .label("Clear all")
            .icon(IconName::Delete)
            .ghost()
            .small()
            .on_click(answer(HistoryPage::ask))
            .into_any_element();
    }
    let delete = {
        let view = view.clone();
        move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
            if view.update(cx, |page, _| page.confirm()) {
                if let Err(e) = UiState::handle(cx).history().clear() {
                    log::error!("clearing history: {e}");
                }
                cx.refresh_windows();
            }
        }
    };
    h_flex()
        .gap_2()
        .child(div().pl_2().text_sm().child(question(count)))
        .child(
            Button::new("keep-history")
                .label("Cancel")
                .ghost()
                .small()
                .on_click(answer(HistoryPage::keep)),
        )
        .child(
            Button::new("delete-history")
                .label("Delete")
                .danger()
                .small()
                .on_click(delete),
        )
        .into_any_element()
}

/// What Clear all asks before deleting `count` transcripts.
fn question(count: usize) -> String {
    if count == 1 {
        "Delete 1 transcript? This can't be undone.".into()
    } else {
        format!("Delete all {count} transcripts? This can't be undone.")
    }
}

/// [`LIMITS`], and in order among them `limit` when it was written by hand.
fn limit_options(limit: usize) -> Vec<(SharedString, SharedString)> {
    let mut limits = LIMITS.to_vec();
    if !limits.contains(&limit) {
        limits.push(limit);
        limits.sort_unstable();
    }
    limits
        .into_iter()
        .map(|limit| {
            let limit = SharedString::from(limit.to_string());
            (limit.clone(), limit)
        })
        .collect()
}

fn entries(cx: &App) -> impl IntoElement + use<> {
    let handle = UiState::handle(cx);
    let entries = handle.history().entries();
    let muted = cx.theme().muted_foreground;
    let empty = if !config(handle).history.enabled {
        Some("History is off: new transcripts are not kept.")
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
    // GPUI measures text unwrapped: without `min_w_0` a long transcript
    // keeps its widest size and pushes Copy past the window's edge.
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
                .min_w_0()
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
            div().flex_shrink_0().child(
                Clipboard::new(("copy-history", ix))
                    .tooltip("Copy")
                    .value(entry.text.clone()),
            ),
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
    use super::{HistoryPage, ago, limit_options, question};

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

    #[test]
    fn entries_kept_offers_presets_and_a_hand_written_limit() {
        let values = |limit| {
            limit_options(limit)
                .into_iter()
                .map(|(value, _)| value.to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(values(200), ["50", "200", "1000"]);
        assert_eq!(values(500), ["50", "200", "500", "1000"]);
        assert_eq!(values(5000), ["50", "200", "1000", "5000"]);
    }

    #[test]
    fn clear_all_deletes_only_once_asked() {
        let mut page = HistoryPage::default();
        assert!(!page.confirm(), "never a one-click wipe");
        page.ask();
        page.keep();
        assert!(!page.confirm(), "Cancel keeps everything");
        page.ask();
        assert!(page.confirm());
        assert!(!page.asking, "answered");
        assert!(!page.confirm());
    }

    #[test]
    fn clear_all_says_how_many_go() {
        assert_eq!(
            question(12),
            "Delete all 12 transcripts? This can't be undone."
        );
        assert_eq!(question(1), "Delete 1 transcript? This can't be undone.");
    }
}
