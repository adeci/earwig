//! The small on-screen pill shown while dictating.

use std::sync::Arc;
use std::time::{Duration, Instant};

use earwig_core::config::{OverlayConfig, OverlayPosition};
use earwig_core::desktop::DesktopCaps;
use gpui_kit::component::{ActiveTheme as _, Theme};
use gpui_kit::layer_shell::{Anchor, KeyboardInteractivity, Layer, LayerShellOptions};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Bounds, Context, Div, FontFeatures, FontWeight, HighlightStyle, Hsla, IntoElement,
    ParentElement as _, Pixels, PlatformDisplay, Point, Render, SharedString, Size, Styled as _,
    StyledText, Subscription, Window, WindowBackgroundAppearance, WindowBounds, WindowDecorations,
    WindowKind, WindowOptions, canvas, div, point, px, size,
};

use crate::ui::accent;
use crate::ui::now_ms;
use crate::ui::pill::{self, Clock, Edge, Look, Motion};

/// Width of the pill.
const WIDTH: f32 = 560.;
/// Gap between the pill and the screen edge it is anchored to.
const MARGIN: f32 = 48.;
/// Transparent room around the pill for its moving edge and its shadow.
const INSET: f32 = pill::ROOM;
/// Gap between the surface and the screen edge it is anchored to.
const SURFACE_MARGIN: f32 = MARGIN - INSET;
const LINE_HEIGHT: f32 = 22.;
/// Frame pacing: smooth while the voice moves the edge, slow when it only
/// breathes, so the overlay never competes with CPU decoding at display rate.
const SPEAKING_FRAME: Duration = Duration::from_millis(16);
const QUIET_FRAME: Duration = Duration::from_millis(50);
const HEADER_HEIGHT: f32 = 22.;
const PADDING_X: f32 = 18.;
const PADDING_Y: f32 = 14.;
const GAP: f32 = 8.;

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
    let window_size = size(px(WIDTH + 2. * INSET), px(window_height(config.max_lines)));
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
        let edge = px(SURFACE_MARGIN);
        let (anchor, margin) = match config.position {
            OverlayPosition::Bottom => (Anchor::BOTTOM, (px(0.), px(0.), edge, px(0.))),
            OverlayPosition::Top => (Anchor::TOP, (edge, px(0.), px(0.), px(0.))),
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

/// Top-left corner that centres a `window` on `screen`, `SURFACE_MARGIN`
/// away from the edge at `position`.
fn popup_origin(
    screen: Bounds<Pixels>,
    window: Size<Pixels>,
    position: OverlayPosition,
) -> Point<Pixels> {
    let x = screen.origin.x + (screen.size.width - window.width) / 2.;
    let y = match position {
        OverlayPosition::Bottom => {
            screen.origin.y + screen.size.height - window.height - px(SURFACE_MARGIN)
        }
        OverlayPosition::Top => screen.origin.y + px(SURFACE_MARGIN),
    };
    point(x, y)
}

/// Room for the header, `max_lines` of text and the inset around the pill;
/// the pill inside shrinks to its content and hugs the anchored edge.
#[allow(clippy::cast_precision_loss)]
fn window_height(max_lines: u32) -> f32 {
    2. * INSET + PADDING_Y * 2. + HEADER_HEIGHT + GAP + LINE_HEIGHT * max_lines.max(1) as f32
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
    /// Latest microphone level.
    level: f32,
    clock: Clock,
    motion: Motion,
    frame_pending: bool,
    /// The pill's colours as they fade toward the current phase's.
    look: Option<Look>,
    tabular_numbers: FontFeatures,
    _appearance: Subscription,
}

impl OverlayView {
    pub fn new(
        max_lines: u32,
        position: OverlayPosition,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let appearance = cx.observe_window_appearance(window, |_, window, cx| {
            Theme::sync_system_appearance(Some(window), cx);
        });
        Self {
            max_lines: max_lines.max(1),
            position,
            phase: Phase::Recording { latched: false },
            started_at_ms: now_ms(),
            stopped_at_ms: None,
            committed: String::new(),
            tentative: String::new(),
            level: 0.,
            clock: Clock::default(),
            motion: Motion::default(),
            frame_pending: false,
            look: None,
            tabular_numbers: FontFeatures(Arc::new(vec![("tnum".into(), 1)])),
            _appearance: appearance,
        }
    }

    /// Recording (re)started or its latch changed. A different start time is a
    /// new recording and clears the previous one's text.
    pub fn recording(&mut self, latched: bool, started_at_ms: u64) {
        if started_at_ms != self.started_at_ms || !self.is_recording() {
            self.committed.clear();
            self.tentative.clear();
            self.level = 0.;
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
        self.level = level.clamp(0., 1.);
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

    /// Accent hairline while listening (brighter with the voice), a neutral
    /// one once listening stops, and a quiet tint of the outcome at the end.
    fn target_look(&self, theme: &Theme, accent: Hsla, energy: f32) -> Look {
        let dark = theme.is_dark();
        // Dark surfaces read as raised when slightly lighter than the page.
        let fill = if dark {
            theme.background.blend(theme.foreground.opacity(0.05))
        } else {
            theme.background
        };
        let (fill, border) = match self.phase {
            Phase::Recording { .. } => (fill, accent.opacity(0.55 + 0.45 * energy)),
            Phase::Finalizing => (fill, theme.border),
            Phase::Done(Outcome::Inserted) => (pill::mix(fill, theme.success, 0.08), theme.success),
            Phase::Done(Outcome::Failed { .. }) => {
                (pill::mix(fill, theme.danger, 0.08), theme.danger)
            }
        };
        Look {
            fill: fill.opacity(0.97),
            border,
            dark,
        }
    }

    fn header(&self, theme: &Theme) -> Div {
        let (dot, status): (Hsla, Option<SharedString>) = match self.phase {
            Phase::Recording { .. } => (theme.danger, None),
            Phase::Finalizing => (theme.warning, Some("Finishing…".into())),
            Phase::Done(Outcome::Inserted) => (theme.success, Some("Inserted".into())),
            Phase::Done(Outcome::Failed { .. }) => (theme.danger, Some("Not inserted".into())),
        };
        let latched = matches!(self.phase, Phase::Recording { latched: true });
        let words = word_count(&self.committed) + word_count(&self.tentative);
        div()
            .flex()
            .items_center()
            .gap(px(10.))
            .h(px(HEADER_HEIGHT))
            .text_size(px(13.))
            .text_color(theme.muted_foreground)
            .child(div().size(px(8.)).rounded_full().bg(dot))
            .child(
                div()
                    .text_color(theme.foreground)
                    .font_weight(FontWeight::MEDIUM)
                    .font_features(self.tabular_numbers.clone())
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
    }

    fn body(&self, theme: &Theme) -> Div {
        match self.phase {
            Phase::Done(Outcome::Failed { on_clipboard }) => {
                div().text_color(theme.foreground).child(if on_clipboard {
                    "Couldn't type into the app. The text is on the clipboard."
                } else {
                    "Couldn't insert the text. It is saved in History."
                })
            }
            _ if self.committed.is_empty() && self.tentative.is_empty() => div()
                .text_color(theme.muted_foreground)
                .child(if self.is_recording() {
                    "Listening…"
                } else {
                    ""
                }),
            _ => div().text_color(theme.foreground).child(live_text(
                &self.committed,
                &self.tentative,
                theme.muted_foreground,
            )),
        }
    }

    /// Re-render after `interval`; the window closing drops the entity and
    /// ends the loop.
    fn schedule_frame(&mut self, interval: Duration, cx: &mut Context<Self>) {
        if self.frame_pending {
            return;
        }
        self.frame_pending = true;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(interval).await;
            let _ = this.update(cx, |view, cx| {
                view.frame_pending = false;
                cx.notify();
            });
        })
        .detach();
    }
}

impl Render for OverlayView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let dt = self.clock.tick(Instant::now());
        let edge = self
            .motion
            .step(dt, self.is_recording().then_some(self.level));
        let edge = if cx.reduce_motion() {
            Edge::default()
        } else {
            edge
        };
        let frame = if edge.energy > 0.05 {
            SPEAKING_FRAME
        } else {
            QUIET_FRAME
        };
        self.schedule_frame(frame, cx);
        let accent = accent::get(cx);
        let theme = cx.theme();
        let target = self.target_look(theme, accent.unwrap_or(theme.primary), edge.energy);
        let look = self.look.map_or(target, |look| look.fade_to(target, dt));
        self.look = Some(look);
        let header = self.header(theme);
        let body = self.body(theme);

        // The pill hugs the anchored edge; the rest of the window is
        // transparent (and, like all of it, click-through).
        div()
            .size_full()
            .p(px(INSET))
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
                    .px(px(PADDING_X))
                    .py(px(PADDING_Y))
                    .child(
                        canvas(
                            |_, _, _| {},
                            move |bounds, (), window, _| pill::paint(bounds, edge, look, window),
                        )
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full(),
                    )
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
        let window = size(px(WIDTH + 2. * INSET), px(window_height(3)));
        let bottom = popup_origin(screen, window, OverlayPosition::Bottom);
        assert_eq!(bottom.x + px(INSET), px(1000. + (1920. - WIDTH) / 2.));
        // The pill itself sits `MARGIN` from the edge, its inset inside that.
        assert_eq!(bottom.y + window.height - px(INSET), px(1080. - MARGIN));
        let top = popup_origin(screen, window, OverlayPosition::Top);
        assert_eq!(top, point(bottom.x, px(MARGIN - INSET)));
    }
}
