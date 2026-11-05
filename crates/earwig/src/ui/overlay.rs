//! The small on-screen pill shown while dictating.

use std::collections::VecDeque;
use std::time::Duration;

use earwig_core::config::{OverlayConfig, OverlayPosition};
use earwig_core::desktop::DesktopCaps;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::layer_shell::{Anchor, KeyboardInteractivity, Layer, LayerShellOptions};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Bounds, Context, FontWeight, HighlightStyle, Hsla, IntoElement, ParentElement as _, Pixels,
    PlatformDisplay, Point, Render, SharedString, Size, Styled as _, StyledText, Task, Window,
    WindowBackgroundAppearance, WindowBounds, WindowDecorations, WindowKind, WindowOptions, div,
    point, px, size,
};

use crate::ui::now_ms;

const WIDTH: f32 = 560.;
/// Gap between the pill and the screen edge it is anchored to.
const MARGIN: f32 = 48.;
const LINE_HEIGHT: f32 = 22.;
const HEADER_HEIGHT: f32 = 22.;
const PADDING: f32 = 14.;
const GAP: f32 = 8.;
/// Waveform bars, newest on the right.
const LEVEL_BARS: usize = 36;

/// Window options for the overlay, the one place the platform choice is made.
///
/// With layer-shell the pill floats on the Overlay layer of the output the
/// compositor picks (the focused one), never takes keyboard focus, and
/// reserves no space. Elsewhere it is a borderless popup centred on the
/// primary display.
pub fn overlay_window_options(
    caps: &DesktopCaps,
    config: &OverlayConfig,
    display: Option<&dyn PlatformDisplay>,
) -> WindowOptions {
    let window_size = size(px(WIDTH), px(window_height(config.max_lines)));
    let common = WindowOptions {
        titlebar: None,
        focus: false,
        show: true,
        is_movable: false,
        is_resizable: false,
        is_minimizable: false,
        app_id: Some("earwig-overlay".into()),
        window_background: WindowBackgroundAppearance::Transparent,
        window_decorations: Some(WindowDecorations::Client),
        ..WindowOptions::default()
    };
    if caps.layer_shell {
        let (anchor, margin) = match config.position {
            OverlayPosition::Bottom => (Anchor::BOTTOM, (px(0.), px(0.), px(MARGIN), px(0.))),
            OverlayPosition::Top => (Anchor::TOP, (px(MARGIN), px(0.), px(0.), px(0.))),
        };
        return WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds {
                origin: point(px(0.), px(0.)),
                size: window_size,
            })),
            kind: WindowKind::LayerShell(LayerShellOptions {
                namespace: "earwig".into(),
                layer: Layer::Overlay,
                // One edge only: layer-shell centres the surface along it.
                anchor,
                exclusive_zone: None,
                exclusive_edge: None,
                margin: Some(margin),
                keyboard_interactivity: KeyboardInteractivity::None,
            }),
            ..common
        };
    }
    let origin = display.map_or(point(px(0.), px(0.)), |display| {
        popup_origin(display.bounds(), window_size, config.position)
    });
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds {
            origin,
            size: window_size,
        })),
        kind: WindowKind::PopUp,
        display_id: display.map(PlatformDisplay::id),
        ..common
    }
}

/// Top-left corner that centres a `window` on `screen`, `MARGIN` away from
/// the edge at `position`.
fn popup_origin(
    screen: Bounds<Pixels>,
    window: Size<Pixels>,
    position: OverlayPosition,
) -> Point<Pixels> {
    let x = screen.origin.x + (screen.size.width - window.width) / 2.;
    let y = match position {
        OverlayPosition::Bottom => {
            screen.origin.y + screen.size.height - window.height - px(MARGIN)
        }
        OverlayPosition::Top => screen.origin.y + px(MARGIN),
    };
    point(x, y)
}

/// Room for the header and `max_lines` of text; the pill inside shrinks to
/// its content and hugs the anchored edge.
#[allow(clippy::cast_precision_loss)]
fn window_height(max_lines: u32) -> f32 {
    PADDING * 2. + HEADER_HEIGHT + GAP + LINE_HEIGHT * max_lines.max(1) as f32
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Inserted,
    Failed { on_clipboard: bool },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Recording {
        latched: bool,
    },
    /// Recording stopped at `stopped_at_ms`; final text on its way.
    Finalizing,
    Done(Outcome),
}

pub struct OverlayView {
    max_lines: u32,
    position: OverlayPosition,
    phase: Phase,
    started_at_ms: u64,
    stopped_at_ms: Option<u64>,
    committed: String,
    tentative: String,
    levels: VecDeque<f32>,
    _ticker: Task<()>,
}

impl OverlayView {
    pub fn new(max_lines: u32, position: OverlayPosition, cx: &mut Context<Self>) -> Self {
        // Keep the elapsed time moving while recording.
        let ticker = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(250))
                    .await;
                let alive = this.update(cx, |view, cx| {
                    if view.is_recording() {
                        cx.notify();
                    }
                });
                if alive.is_err() {
                    break;
                }
            }
        });
        Self {
            max_lines: max_lines.max(1),
            position,
            phase: Phase::Recording { latched: false },
            started_at_ms: now_ms(),
            stopped_at_ms: None,
            committed: String::new(),
            tentative: String::new(),
            levels: VecDeque::with_capacity(LEVEL_BARS),
            _ticker: ticker,
        }
    }

    /// Recording (re)started or its latch changed. A different start time is a
    /// new recording and clears the previous one's text.
    pub fn recording(&mut self, latched: bool, started_at_ms: u64) {
        if started_at_ms != self.started_at_ms || !self.is_recording() {
            self.committed.clear();
            self.tentative.clear();
            self.levels.clear();
            self.stopped_at_ms = None;
            self.started_at_ms = started_at_ms;
        }
        self.phase = Phase::Recording { latched };
    }

    pub fn finalizing(&mut self) {
        self.phase = Phase::Finalizing;
        self.stopped_at_ms.get_or_insert_with(now_ms);
    }

    pub fn finish(&mut self, outcome: Outcome) {
        self.stopped_at_ms.get_or_insert_with(now_ms);
        self.phase = Phase::Done(outcome);
    }

    pub fn set_text(&mut self, committed: String, tentative: String) {
        self.committed = committed;
        self.tentative = tentative;
    }

    pub fn push_level(&mut self, level: f32) {
        if self.levels.len() == LEVEL_BARS {
            self.levels.pop_front();
        }
        self.levels.push_back(level.clamp(0., 1.));
    }

    pub fn is_recording(&self) -> bool {
        matches!(self.phase, Phase::Recording { .. })
    }

    pub fn has_outcome(&self) -> bool {
        matches!(self.phase, Phase::Done(_))
    }

    fn elapsed(&self) -> Duration {
        let end = self.stopped_at_ms.unwrap_or_else(now_ms);
        Duration::from_millis(end.saturating_sub(self.started_at_ms))
    }

    fn render_waveform(&self, color: Hsla) -> impl IntoElement {
        let missing = LEVEL_BARS - self.levels.len();
        let levels = std::iter::repeat_n(0., missing).chain(self.levels.iter().copied());
        div()
            .flex()
            .items_center()
            .gap(px(2.))
            .h(px(HEADER_HEIGHT))
            .children(levels.map(|level| {
                // Speech levels sit low on a linear scale; lift them a bit.
                let height = 2. + level.sqrt() * (HEADER_HEIGHT - 4.);
                div().w(px(3.)).h(px(height)).rounded(px(1.5)).bg(color)
            }))
    }
}

impl Render for OverlayView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let foreground = theme.foreground;
        let muted = theme.muted_foreground;
        let (dot, status): (Hsla, Option<SharedString>) = match self.phase {
            Phase::Recording { .. } => (theme.danger, None),
            Phase::Finalizing => (theme.warning, Some("Finishing…".into())),
            Phase::Done(Outcome::Inserted) => (theme.success, Some("Inserted".into())),
            Phase::Done(Outcome::Failed { .. }) => (theme.danger, Some("Not inserted".into())),
        };
        let latched = matches!(self.phase, Phase::Recording { latched: true });
        let words = word_count(&self.committed) + word_count(&self.tentative);

        let header = div()
            .flex()
            .items_center()
            .gap(px(10.))
            .h(px(HEADER_HEIGHT))
            .text_size(px(13.))
            .text_color(muted)
            .child(div().size(px(10.)).rounded_full().bg(dot))
            .child(
                div()
                    .text_color(foreground)
                    .font_weight(FontWeight::MEDIUM)
                    .child(format_elapsed(self.elapsed())),
            )
            .child(format!(
                "{words} {}",
                if words == 1 { "word" } else { "words" }
            ))
            .when(latched, |this| {
                this.child(
                    div()
                        .px(px(6.))
                        .rounded(px(4.))
                        .border_1()
                        .border_color(theme.border)
                        .text_size(px(11.))
                        .child("latched"),
                )
            })
            .when_some(status, |this, status| {
                this.child(div().text_color(dot).child(status))
            })
            .child(div().flex_1())
            .child(self.render_waveform(if self.is_recording() { dot } else { muted }));

        let body = match self.phase {
            Phase::Done(Outcome::Failed { on_clipboard }) => {
                div().text_color(foreground).child(if on_clipboard {
                    "Couldn't type into the app. The text is on the clipboard."
                } else {
                    "Couldn't insert the text. It is saved in History."
                })
            }
            _ if self.committed.is_empty() && self.tentative.is_empty() => {
                div().text_color(muted).child(if self.is_recording() {
                    "Listening…"
                } else {
                    ""
                })
            }
            _ => div().text_color(foreground).child(live_text(
                &self.committed,
                &self.tentative,
                muted,
            )),
        };

        // The pill hugs the anchored edge; the rest of the window is
        // transparent (and, like all of it, click-through).
        div()
            .size_full()
            .flex()
            .flex_col()
            .map(|this| match self.position {
                OverlayPosition::Bottom => this.justify_end(),
                OverlayPosition::Top => this.justify_start(),
            })
            .child(
                div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .gap(px(GAP))
                    .p(px(PADDING))
                    .rounded(px(18.))
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.background.opacity(0.96))
                    .child(header)
                    .child(
                        // Only the last `max_lines` wrapped lines: anchor the
                        // text to the bottom and clip what scrolls off the top.
                        #[allow(clippy::cast_precision_loss)]
                        div()
                            .max_h(px(LINE_HEIGHT * self.max_lines as f32))
                            .overflow_hidden()
                            .flex()
                            .flex_col()
                            .justify_end()
                            .text_size(px(15.))
                            .line_height(px(LINE_HEIGHT))
                            .child(body),
                    ),
            )
    }
}

/// Committed text in full colour followed by the tentative tail dimmed.
fn live_text(committed: &str, tentative: &str, dim: Hsla) -> StyledText {
    let separator = if committed.is_empty()
        || tentative.is_empty()
        || committed.ends_with(char::is_whitespace)
        || tentative.starts_with(char::is_whitespace)
    {
        ""
    } else {
        " "
    };
    let text = format!("{committed}{separator}{tentative}");
    let tail = committed.len() + separator.len()..text.len();
    StyledText::new(text).with_highlights([(
        tail,
        HighlightStyle {
            color: Some(dim),
            ..HighlightStyle::default()
        },
    )])
}

fn word_count(text: &str) -> usize {
    text.split_whitespace().count()
}

fn format_elapsed(elapsed: Duration) -> String {
    let secs = elapsed.as_secs();
    format!("{}:{:02}", secs / 60, secs % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elapsed_rolls_over_minutes() {
        assert_eq!(format_elapsed(Duration::from_millis(59_999)), "0:59");
        assert_eq!(format_elapsed(Duration::from_secs(61)), "1:01");
        assert_eq!(format_elapsed(Duration::from_secs(600)), "10:00");
    }

    #[test]
    fn popup_is_centred_and_keeps_its_margin_from_the_anchored_edge() {
        let screen = Bounds {
            origin: point(px(1000.), px(0.)),
            size: size(px(1920.), px(1080.)),
        };
        let window = size(px(WIDTH), px(100.));
        let bottom = popup_origin(screen, window, OverlayPosition::Bottom);
        assert_eq!(bottom.x, px(1000. + (1920. - WIDTH) / 2.));
        assert_eq!(bottom.y + window.height, px(1080. - MARGIN));
        let top = popup_origin(screen, window, OverlayPosition::Top);
        assert_eq!(top, point(bottom.x, px(MARGIN)));
    }
}
