//! The Opacity slider, and a preview pill that ripples briefly whenever its look changes.

use std::f32::consts::TAU;
use std::time::{Duration, Instant};

use earwig_core::config::OverlayConfig;
use earwig_core::spectrum::{BANDS, Spectrum};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::slider::{Slider, SliderState};
use gpui_kit::{
    App, AppContext as _, Context, Div, Entity, IntoElement, ParentElement as _, Render,
    Styled as _, Subscription, Task, Window, canvas, checkerboard, div, px,
};

use crate::ui::overlay;
use crate::ui::pill::{self, Clock, Edge, Look, Motion};
use crate::ui::{UiState, accent, config, edit_config};

/// What the preview's pill says, and how long it has been listening.
const SAMPLE: &str = "Your words appear here as you speak.";
const SAMPLE_ELAPSED: Duration = Duration::from_secs(3);
/// Room for the edge's fullest swell, and a bit.
const MARGIN: f32 = pill::DEPTH + 8.;
/// Checkerboard square size; the checks show through the fill.
const CHECKER: f32 = 8.;
/// The overlay's pacing while the voice moves the pill.
const FRAME: Duration = Duration::from_millis(16);
/// Keeps speaking this long after a change, bridging the gaps in a slider drag.
const HOLD: Duration = Duration::from_millis(150);
/// Below this reach (px) the preview counts as still.
const SETTLED: f32 = 0.3;
/// Writes wait until a slider has rested this long.
const SAVE_DELAY: Duration = Duration::from_millis(300);
/// Syllables per second of the synthetic voice.
const SYLLABLE_RATE: f32 = 4.5;

pub struct PillPreview {
    opacity_slider: Entity<SliderState>,
    /// The slider's value as of the last change seen.
    opacity: f32,
    save: Option<Task<()>>,
    /// Only the config file sets it; read once.
    border: f32,
    demo: Demo,
    frame_pending: bool,
    _slider: Subscription,
}

impl PillPreview {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let overlay = config(UiState::handle(cx)).overlay;
        let opacity = overlay.opacity();
        let opacity_slider = cx.new(|_| {
            SliderState::new()
                .min(OverlayConfig::MIN_OPACITY)
                .max(1.)
                .step(0.01)
                .default_value(opacity)
        });
        // Observed rather than subscribed: assistive tech changes the value without an event.
        let observed = cx.observe(&opacity_slider, |this, _, cx| this.slid(cx));
        Self {
            opacity_slider,
            opacity,
            save: None,
            border: overlay.border(),
            demo: Demo::default(),
            frame_pending: false,
            _slider: observed,
        }
    }

    /// Padded for the half-thumb overhanging each end.
    pub fn slider(view: &Entity<Self>, disabled: bool, cx: &App) -> Div {
        div()
            .px_2()
            .child(Slider::new(&view.read(cx).opacity_slider).disabled(disabled))
    }

    /// Saves once the slider rests; a newer change replaces (cancels) the pending write.
    fn slid(&mut self, cx: &mut Context<Self>) {
        let value = self.opacity_slider.read(cx).value().end();
        if (value - self.opacity).abs() <= f32::EPSILON {
            return;
        }
        self.opacity = value;
        let saved = (value * 100.).round() / 100.;
        self.save = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SAVE_DELAY).await;
            let _ = this.update(cx, |_, cx| edit_config(cx, |c| c.overlay.opacity = saved));
        }));
        cx.notify();
    }

    /// Re-renders after `FRAME`, unless the window has closed.
    fn schedule_frame(&mut self, cx: &mut Context<Self>) {
        if self.frame_pending {
            return;
        }
        self.frame_pending = true;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(FRAME).await;
            let _ = this.update(cx, |this, cx| {
                this.frame_pending = false;
                cx.notify();
            });
        })
        .detach();
    }
}

impl Render for PillPreview {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let now = Instant::now();
        let look = Look::themed(cx.theme(), accent::get(cx), self.opacity, self.border);
        self.demo.show(look, now);
        let edge = if pill::holds_still(window, cx) {
            self.demo.settle();
            Edge::default()
        } else if let Some(edge) = self.demo.frame(now) {
            self.schedule_frame(cx);
            edge
        } else {
            Edge::default()
        };
        let theme = cx.theme();
        // Laid out like the overlay's pill, as wide as the row allows.
        let sample = div()
            .w_full()
            .max_w(px(overlay::WIDTH))
            .flex()
            .flex_col()
            .gap(px(overlay::GAP))
            .px(px(overlay::PADDING_X))
            .py(px(overlay::PADDING_Y))
            .child(
                canvas(
                    |_, _, _| {},
                    move |bounds, (), window, _| pill::paint(bounds, &edge, look, 1., window),
                )
                .absolute()
                .top_0()
                .left_0()
                .size_full(),
            )
            .child(overlay::header_row(
                theme,
                SAMPLE_ELAPSED,
                SAMPLE.split_whitespace().count(),
                None,
            ))
            .child(
                div()
                    .text_size(px(overlay::TEXT_SIZE))
                    .line_height(px(overlay::LINE_HEIGHT))
                    .text_color(theme.foreground)
                    .child(SAMPLE),
            );
        // The backdrop clips the pill's edge and shadow; its border stays on top.
        let radius = theme.radius_lg;
        let checks = checkerboard(
            theme.muted_foreground.opacity(0.3),
            CHECKER * window.scale_factor(),
        );
        div()
            .w_full()
            .flex()
            .justify_center()
            .p(px(MARGIN))
            .rounded(radius)
            .bg(theme.background)
            .overflow_hidden()
            .child(div().absolute().inset_0().rounded(radius).bg(checks))
            .child(sample)
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .rounded(radius)
                    .border_1()
                    .border_color(theme.border),
            )
    }
}

/// A synthetic voice speaks briefly on each look change; then the pill settles.
#[derive(Default)]
struct Demo {
    motion: Motion,
    clock: Clock,
    look: Option<Look>,
    /// When the look last changed; `None` once the pill has settled.
    moved_at: Option<Instant>,
    /// Seconds spoken so far, so each change continues the sentence.
    spoken: f32,
}

impl Demo {
    /// Show `look` from `now` on, speaking if it differs from the last.
    fn show(&mut self, look: Look, now: Instant) {
        if self.look.replace(look).is_some_and(|last| last != look) {
            self.moved_at = Some(now);
        }
    }

    fn settle(&mut self) {
        self.moved_at = None;
        self.clock = Clock::default();
    }

    /// The pill's edge at `now`, or `None` when it is still.
    fn frame(&mut self, now: Instant) -> Option<Edge> {
        let moved_at = self.moved_at?;
        let dt = self.clock.tick(now);
        let speaking = now.saturating_duration_since(moved_at) < HOLD;
        let voice = speaking.then(|| {
            self.spoken += dt;
            synthetic_voice(self.spoken)
        });
        let edge = self.motion.step(dt, voice.as_ref());
        if !speaking && edge.reach() < SETTLED {
            self.settle();
            return None;
        }
        Some(edge)
    }
}

/// A mic level (0..1 over -60..0 dBFS) like steady talking, some syllables stressed.
fn synthetic_level(t: f32) -> f32 {
    let syllable = (TAU * SYLLABLE_RATE * t).sin().max(0.).sqrt();
    let stress = 0.8 + 0.2 * (TAU * 0.7 * t).sin();
    0.3 + 0.5 * syllable * stress
}

/// As speech bands: a low fundamental and two drifting formants.
#[allow(clippy::cast_precision_loss)]
fn synthetic_voice(t: f32) -> Spectrum {
    let level = synthetic_level(t);
    let loudness = ((level - 0.3) / 0.5).clamp(0., 1.);
    let first = 0.3 + 0.08 * (TAU * 1.3 * t).sin();
    let second = 0.62 + 0.12 * (TAU * 0.9 * t + 1.).sin();
    let bump = |x: f32, centre: f32, width: f32| (-((x - centre) / width).powi(2)).exp();
    Spectrum {
        level,
        bands: std::array::from_fn(|band| {
            let x = band as f32 / (BANDS - 1) as f32;
            let shape = 0.6 * bump(x, 0.05, 0.12)
                + 0.9 * bump(x, first, 0.1)
                + 0.6 * bump(x, second, 0.12)
                + 0.12;
            (shape * loudness).clamp(0., 1.)
        }),
    }
}

#[cfg(test)]
mod tests {
    use gpui_kit::Hsla;

    use super::*;

    /// The pill at fill `opacity` on the dark theme.
    fn look(opacity: f32) -> Look {
        Look {
            fill: Hsla::black().opacity(opacity),
            accent: Hsla::white(),
            dark: true,
            depth: 1.,
            border: 0.,
        }
    }

    /// `frames` frames at 60 Hz from `start`, showing `look_at(frame)`.
    fn run(
        demo: &mut Demo,
        start: Instant,
        frames: u32,
        look_at: impl Fn(u32) -> Look,
    ) -> Vec<Option<Edge>> {
        (0..frames)
            .map(|i| {
                let now = start + Duration::from_secs(1) * i / 60;
                demo.show(look_at(i), now);
                demo.frame(now)
            })
            .collect()
    }

    #[test]
    #[allow(clippy::cast_precision_loss)]
    fn preview_speaks_while_a_slider_moves_then_settles() {
        // Seeded: how long it takes to settle depends on how the kicks vary.
        let mut demo = Demo {
            motion: Motion::seeded(1),
            ..Demo::default()
        };
        // A slider dragged a step every frame for a second, then let go.
        let frames = run(&mut demo, Instant::now(), 200, |i| {
            look(0.5 + 0.005 * i.min(60) as f32)
        });
        assert!(frames[0].is_none(), "the first look is no change");
        let reach = frames[..=60]
            .iter()
            .map(|f| f.map_or(0., |e| e.reach()))
            .fold(0., f32::max);
        assert!(reach > 5., "moves while the slider does: {reach}");
        let settled_at = 1 + frames[1..].iter().position(Option::is_none).unwrap();
        let after_last_change = (settled_at - 60) as f32 / 60.;
        assert!(
            (0.4..=1.8).contains(&after_last_change),
            "eases out after the slider stops: {after_last_change} s"
        );
        assert!(frames[settled_at..].iter().all(Option::is_none));
    }

    #[test]
    fn each_new_look_ripples_the_water_once() {
        let dark = look(OverlayConfig::DEFAULT_OPACITY);
        let light = Look {
            fill: Hsla::white().opacity(OverlayConfig::DEFAULT_OPACITY),
            dark: false,
            ..dark
        };
        let mut demo = Demo::default();
        let mut start = Instant::now();
        demo.show(dark, start);
        // Switching after each settle lands somewhere else in the synthetic voice's sentence.
        for switch in 0..8 {
            let shown = if switch % 2 == 0 { light } else { dark };
            let frames = run(&mut demo, start, 240, |_| shown);
            let reach = frames.iter().flatten().map(Edge::reach).fold(0., f32::max);
            assert!(
                reach > 4.,
                "switch {switch} moves the preview's edge by {reach} px"
            );
            let settled = frames.iter().position(Option::is_none).unwrap();
            assert!(frames[settled..].iter().all(Option::is_none), "{switch}");
            start += Duration::from_secs(4);
        }
    }
}
