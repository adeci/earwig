//! The bottom of the sidebar: this earwig's version and, where it updates
//! itself, a newer one: the button that installs it, how that goes, and why
//! it failed.

use earwig_core::events::{Command, UpdateState};
use earwig_core::update;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::progress::Progress;
use gpui_kit::component::{ActiveTheme as _, Sizable as _, v_flex};
use gpui_kit::{AnyElement, App, IntoElement, ParentElement as _, SharedString, Styled as _, div};

use super::models::percent;
use crate::human_size_of;
use crate::ui::UiState;

pub(super) fn footer(cx: &App) -> impl IntoElement + use<> {
    let update = UiState::status(cx).update.clone();
    v_flex()
        .w_full()
        .gap_2()
        .children(update.and_then(|update| newer(&update, cx)))
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(format!("earwig {}", update::VERSION)),
        )
}

/// What there is to show about a newer release.
fn newer(update: &UpdateState, cx: &App) -> Option<AnyElement> {
    let muted = cx.theme().muted_foreground;
    Some(match update {
        UpdateState::None => return None,
        UpdateState::Available {
            version,
            installable: true,
        } => install_button(version).into_any_element(),
        UpdateState::Available {
            version,
            installable: false,
        } => release_link("release-page", format!("{version} is available"), version)
            .into_any_element(),
        UpdateState::Downloading {
            version,
            downloaded,
            total,
        } => v_flex()
            .gap_1()
            .text_xs()
            .text_color(muted)
            .child(format!("Downloading {version}"))
            .child(
                Progress::new("update-progress")
                    .value(percent(*downloaded, *total))
                    .xsmall(),
            )
            .child(human_size_of(*downloaded, *total))
            .into_any_element(),
        UpdateState::Installing { version } => v_flex()
            .gap_1()
            .text_xs()
            .text_color(muted)
            .child(format!("Installing {version}"))
            .child(Progress::new("update-progress").loading(true).xsmall())
            .into_any_element(),
        UpdateState::Failed { version, reason } => v_flex()
            .gap_1()
            .child(install_button(version))
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().danger)
                    .child(format!("Couldn't update: {reason}.")),
            )
            .child(release_link("release-page", "Release page".into(), version))
            .into_any_element(),
    })
}

fn install_button(version: &str) -> Button {
    Button::new("update")
        .primary()
        .xsmall()
        .w_full()
        .label(format!("Update to {version}"))
        .on_click(|_, _, cx| UiState::handle(cx).send(Command::Update))
}

/// `label`, opening the release page of `version`.
fn release_link(id: &'static str, label: String, version: &str) -> Button {
    let page = update::release_page(version);
    Button::new(id)
        .link()
        .xsmall()
        .label(SharedString::from(label))
        .on_click(move |_, _, cx| cx.open_url(&page))
}
