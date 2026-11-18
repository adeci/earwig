//! The Replacements page: what earwig heard and what it writes instead, under
//! a row that adds one. A History entry opens the same row beneath itself.

use std::sync::PoisonError;

use earwig_core::replacements::normalize;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::setting::{SettingGroup, SettingItem, SettingPage};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Div, Entity, IntoElement, ParentElement as _,
    Pixels, SharedString, Styled as _, Subscription, Window, div, px,
};

use super::{LOCKED_HINT, spaced};
use crate::ui::{UiState, edit_config};

/// What the sidebar search finds the page by.
const KEYWORDS: [&str; 5] = ["replacements", "words", "names", "spelling", "vocabulary"];
/// Width of the column after the two texts: the Add button, a row's bin.
const ACTION: Pixels = px(72.);

pub struct ReplacementsPage {
    row: AddRow,
    /// The add row under a History entry, or what it added there.
    fix: Option<Fix>,
}

/// Two fields and a button; Enter adds too.
struct AddRow {
    heard: Entity<InputState>,
    written: Entity<InputState>,
    _subscriptions: [Subscription; 2],
}

/// Where an add row is.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Place {
    Page,
    History,
}

/// A History entry, by its time, getting a replacement.
enum Fix {
    Open {
        entry: u64,
        row: AddRow,
    },
    /// The row added one, as this line says.
    Done {
        entry: u64,
        line: SharedString,
    },
}

/// How the table stands on a phrase.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Standing {
    New,
    Mine,
    /// The system configuration sets it.
    Locked,
}

impl AddRow {
    fn new(place: Place, window: &mut Window, cx: &mut Context<ReplacementsPage>) -> Self {
        let mut field = |placeholder: &'static str| {
            let input = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
            let typed = cx.subscribe_in(
                &input,
                window,
                move |page: &mut ReplacementsPage, _, event: &InputEvent, window, cx| match event {
                    InputEvent::Change => cx.notify(),
                    InputEvent::PressEnter { .. } => page.add(place, window, cx),
                    InputEvent::Focus | InputEvent::Blur => {}
                },
            );
            (input, typed)
        };
        let (heard, heard_typed) = field("Heard, like get hub");
        let (written, written_typed) = field("Written, like GitHub");
        Self {
            heard,
            written,
            _subscriptions: [heard_typed, written_typed],
        }
    }
}

impl ReplacementsPage {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            row: AddRow::new(Place::Page, window, cx),
            fix: None,
        }
    }

    /// Open the add row under a History entry, or close the one open there.
    fn fix(&mut self, entry: u64, window: &mut Window, cx: &mut Context<Self>) {
        let open = matches!(&self.fix, Some(Fix::Open { entry: open, .. }) if *open == entry);
        self.fix = (!open).then(|| {
            let row = AddRow::new(Place::History, window, cx);
            row.heard.update(cx, |input, cx| input.focus(window, cx));
            Fix::Open { entry, row }
        });
        cx.notify();
    }

    /// Add what the row at `place` says, or go to the field it still lacks.
    fn add(&mut self, place: Place, window: &mut Window, cx: &mut Context<Self>) {
        let (row, entry) = match (place, &self.fix) {
            (Place::Page, _) => (&self.row, None),
            (Place::History, Some(Fix::Open { entry, row })) => (row, Some(*entry)),
            (Place::History, _) => return,
        };
        let (heard_field, written_field) = (row.heard.clone(), row.written.clone());
        let heard = normalize(&heard_field.read(cx).value());
        let written = written_field.read(cx).value();
        let lacking = if heard.is_empty() {
            Some(&heard_field)
        } else if written.is_empty() {
            Some(&written_field)
        } else {
            None
        };
        if let Some(field) = lacking {
            field.update(cx, |input, cx| input.focus(window, cx));
            return;
        }
        if look_up(&heard, cx) == Standing::Locked {
            return;
        }
        edit_config(cx, |c| {
            c.replacements.insert(heard.clone(), written.to_string());
        });
        // A settings file with an error takes no change: the row stays as typed.
        if !is_written(&heard, &written, cx) {
            return;
        }
        if let Some(entry) = entry {
            let line = format!("earwig now writes “{written}” for “{heard}”.").into();
            self.fix = Some(Fix::Done { entry, line });
        } else {
            for field in [&heard_field, &written_field] {
                field.update(cx, |input, cx| input.set_value("", window, cx));
            }
            heard_field.update(cx, |input, cx| input.focus(window, cx));
        }
        cx.notify();
    }
}

pub fn page(view: &Entity<ReplacementsPage>) -> SettingPage {
    let view = view.clone();
    SettingPage::new("Replacements")
        .icon(IconName::Replace)
        .resettable(false)
        .description("What earwig writes instead of what it heard.")
        .groups(spaced(vec![SettingGroup::new().item(
            SettingItem::render(move |_, _, cx| table(&view, cx)).keywords(KEYWORDS),
        )]))
}

/// The add row, the user's entries with a bin each, then the locked ones.
fn table(view: &Entity<ReplacementsPage>, cx: &App) -> impl IntoElement + use<> {
    let muted = cx.theme().muted_foreground;
    let (locked, mine): (Vec<_>, Vec<_>) =
        entries(cx).into_iter().partition(|(_, _, locked)| *locked);
    let note = |text: &'static str| div().px_3().text_sm().text_color(muted).child(text);
    v_flex()
        .w_full()
        .gap_3()
        .child(add_row(&view.read(cx).row, Place::Page, view, cx))
        .when(locked.is_empty() && mine.is_empty(), |this| {
            this.child(note("No replacements yet."))
        })
        .children(mine.into_iter().map(|(heard, written, _)| {
            let bin = Button::new(SharedString::from(format!("remove-{heard}")))
                .icon(IconName::Trash)
                .ghost()
                .xsmall()
                .tooltip("Remove")
                .on_click({
                    let heard = heard.clone();
                    move |_, _, cx| {
                        edit_config(cx, |c| {
                            c.replacements.remove(heard.as_str());
                        });
                    }
                });
            entry_row(heard, written, Some(bin), cx)
        }))
        .when(!locked.is_empty(), |this| this.child(note(LOCKED_HINT)))
        .children(
            locked
                .into_iter()
                .map(|(heard, written, _)| entry_row(heard, written, None, cx).opacity(0.5)),
        )
}

/// Every entry: heard, written, and whether the system configuration set it.
fn entries(cx: &App) -> Vec<(SharedString, SharedString, bool)> {
    let store = UiState::handle(cx).config();
    let store = store.read().unwrap_or_else(PoisonError::into_inner);
    store
        .config()
        .replacements
        .iter()
        .map(|(heard, written)| {
            (
                heard.into(),
                written.into(),
                store.is_replacement_locked(heard),
            )
        })
        .collect()
}

fn look_up(phrase: &str, cx: &App) -> Standing {
    let store = UiState::handle(cx).config();
    let store = store.read().unwrap_or_else(PoisonError::into_inner);
    if store.is_replacement_locked(phrase) {
        Standing::Locked
    } else if store.config().replacements.contains_key(phrase) {
        Standing::Mine
    } else {
        Standing::New
    }
}

fn is_written(phrase: &str, written: &str, cx: &App) -> bool {
    let store = UiState::handle(cx).config();
    let store = store.read().unwrap_or_else(PoisonError::into_inner);
    store.config().replacements.get(phrase).map(String::as_str) == Some(written)
}

fn entry_row(heard: SharedString, written: SharedString, bin: Option<Button>, cx: &App) -> Div {
    let cell = |text: SharedString| div().flex_1().min_w_0().px_3().text_sm().child(text);
    h_flex()
        .w_full()
        .items_center()
        .gap_2()
        .pb_3()
        .border_b_1()
        .border_color(cx.theme().border)
        .child(cell(heard))
        .child(arrow(cx))
        .child(cell(written))
        .child(
            h_flex()
                .w(ACTION)
                .flex_shrink_0()
                .justify_end()
                .children(bin),
        )
}

fn arrow(cx: &App) -> impl IntoElement + use<> {
    Icon::new(IconName::ArrowRight)
        .small()
        .flex_shrink_0()
        .text_color(cx.theme().muted_foreground)
}

fn add_row(
    row: &AddRow,
    place: Place,
    view: &Entity<ReplacementsPage>,
    cx: &App,
) -> impl IntoElement + use<> {
    let phrase = normalize(&row.heard.read(cx).value());
    let standing = look_up(&phrase, cx);
    let ready =
        !phrase.is_empty() && row.written.read(cx).text().len() > 0 && standing != Standing::Locked;
    let (id, label) = match (place, standing) {
        (Place::Page, Standing::Mine) => ("add-replacement", "Change"),
        (Place::Page, _) => ("add-replacement", "Add"),
        (Place::History, Standing::Mine) => ("add-fix", "Change"),
        (Place::History, _) => ("add-fix", "Add"),
    };
    let view = view.clone();
    v_flex()
        .w_full()
        .gap_1()
        .child(
            h_flex()
                .w_full()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(Input::new(&row.heard).aria_label("Heard")),
                )
                .child(arrow(cx))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(Input::new(&row.written).aria_label("Written")),
                )
                .child(
                    Button::new(id)
                        .label(label)
                        .primary()
                        .w(ACTION)
                        .flex_shrink_0()
                        .disabled(!ready)
                        .on_click(move |_, window, cx| {
                            view.update(cx, |page, cx| page.add(place, window, cx));
                        }),
                ),
        )
        .when(standing == Standing::Locked, |this| {
            this.child(
                div()
                    .px_3()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(LOCKED_HINT),
            )
        })
}

/// On a History entry: opens the add row under it.
pub fn fix_button(
    view: &Entity<ReplacementsPage>,
    entry: u64,
    ix: usize,
) -> impl IntoElement + use<> {
    let view = view.clone();
    Button::new(("fix-history", ix))
        .icon(IconName::Replace)
        .ghost()
        .xsmall()
        .tooltip("Replace words")
        .on_click(move |_, window, cx| {
            view.update(cx, |page, cx| page.fix(entry, window, cx));
        })
}

/// Under a History entry: the add row its button opened, or what that added.
pub fn fix_row(view: &Entity<ReplacementsPage>, entry: u64, cx: &App) -> Option<AnyElement> {
    match view.read(cx).fix.as_ref()? {
        Fix::Open { entry: open, row } if *open == entry => {
            Some(add_row(row, Place::History, view, cx).into_any_element())
        }
        Fix::Done { entry: done, line } if *done == entry => Some(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(line.clone())
                .into_any_element(),
        ),
        _ => None,
    }
}
