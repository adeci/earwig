//! The overlay pill's look settings: the sliders for motion, opacity and
//! border, and a small pill that shows the result, speaking while the
//! motion or the style changes.

use std::f32::consts::TAU;
use std::time::{Duration, Instant};

use earwig_core::config::{Config, OverlayConfig, OverlayStyle};
use earwig_core::spectrum::{BANDS, Spectrum};
use gpui_kit::component::slider::{Slider, SliderState};
use gpui_kit::component::{ActiveTheme as _, h_flex};
use gpui_kit::{
    App, AppContext as _, Bounds, ContentMask, Context, Div, Entity, Hsla, IntoElement,
    ParentElement as _, Pixels, Render, Styled as _, Subscription, Task, Window, canvas, div, fill,
    point, px, size,
};

use crate::ui::pill::{self, Clock, Look, Motion, Shape};
use crate::ui::{UiState, accent, config, edit_config};

/// The preview is the overlay's pill at this size.
const SCALE: f32 = 0.5;
const PILL_WIDTH: f32 = 120.;
const PILL_HEIGHT: f32 = 30.;
/// Checkered backdrop around the preview pill, so its transparency shows,
/// with room above and below for the bars or the edge at the widest motion.
const CARD_MARGIN: f32 = 12.;
const CARD_MARGIN_Y: f32 = pill::MAX_REACH * SCALE + 1.;
const CARD_WIDTH: f32 = PILL_WIDTH + 2. * CARD_MARGIN;
const CARD_HEIGHT: f32 = PILL_HEIGHT + 2. * CARD_MARGIN_Y;
const CHECKER: f32 = 6.;
const SLIDER_WIDTH: f32 = 180.;
/// Same pacing as the overlay while the voice moves the pill.
const FRAME: Duration = Duration::from_millis(16);
/// The preview keeps speaking this long after the motion slider or style
/// last changed, which bridges the gaps between pointer events of one drag.
const HOLD: Duration = Duration::from_millis(150);
/// Unscaled reach below which the preview counts as still: under a third of
/// a preview pixel.
const SETTLED: f32 = 0.3 / SCALE;
/// Writes wait until a slider has rested this long.
const SAVE_DELAY: Duration = Duration::from_millis(300);
/// Syllables per second of the synthetic voice.
const SYLLABLE_RATE: f32 = 4.5;

/// Which of the pill's settings a slider row controls.
#[derive(Clone, Copy)]
pub enum Knob {
    Motion,
    Opacity,
    Border,
}

pub struct PillPreview {
    style: OverlayStyle,
    motion: Dial,
    opacity: Dial,
    border: Dial,
    demo: Demo,
    frame_pending: bool,
    _sliders: [Subscription; 3],
}

/// A slider, its value as of the last change seen, and the pending write.
struct Dial {
    state: Entity<SliderState>,
    value: f32,
    save: Option<Task<()>>,
}

impl PillPreview {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let overlay = config(UiState::handle(cx)).overlay;
        let motion = Dial::new(0., OverlayConfig::MAX_MOTION, 0.1, overlay.motion(), cx);
        let opacity = Dial::new(OverlayConfig::MIN_OPACITY, 1., 0.01, overlay.opacity(), cx);
        let border = Dial::new(0., OverlayConfig::MAX_BORDER, 0.5, overlay.border(), cx);
        // Observed rather than subscribed: assistive-technology steps change
        // the value without emitting an event.
        let sliders = [
            cx.observe(&motion.state, |this, _, cx| {
                if let Some(value) = this.motion.changed(|c, v| c.overlay.motion = v, cx) {
                    this.demo.nudge(value, Instant::now());
                }
            }),
            cx.observe(&opacity.state, |this, _, cx| {
                this.opacity.changed(|c, v| c.overlay.opacity = v, cx);
            }),
            cx.observe(&border.state, |this, _, cx| {
                this.border.changed(|c, v| c.overlay.border = v, cx);
            }),
        ];
        Self {
            style: overlay.style,
            demo: Demo::new(overlay.style, motion.value),
            motion,
            opacity,
            border,
            frame_pending: false,
            _sliders: sliders,
        }
    }

    /// The style select changed: show the new style speaking.
    pub fn set_style(&mut self, style: OverlayStyle, cx: &mut Context<Self>) {
        self.style = style;
        self.demo.motion.set_style(style);
        self.demo.nudge(self.motion.value, Instant::now());
        cx.notify();
    }

    /// A setting row's control: the knob's slider, with the preview beside
    /// the motion slider and room for it beside the others, so the sliders
    /// line up.
    pub fn field(view: &Entity<Self>, knob: Knob, disabled: bool, cx: &App) -> Div {
        let this = view.read(cx);
        let spacer = || div().w(px(CARD_WIDTH)).into_any_element();
        let (slider, beside) = match knob {
            Knob::Motion => (&this.motion.state, view.clone().into_any_element()),
            Knob::Opacity => (&this.opacity.state, spacer()),
            Knob::Border => (&this.border.state, spacer()),
        };
        h_flex()
            .gap_4()
            .items_center()
            .child(
                div()
                    .w(px(SLIDER_WIDTH))
                    .child(Slider::new(slider).disabled(disabled)),
            )
            .child(beside)
    }

    /// Re-render after `FRAME`; the window closing drops the entity and ends
    /// the loop.
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

impl Dial {
    fn new(min: f32, max: f32, step: f32, value: f32, cx: &mut Context<PillPreview>) -> Self {
        let state = cx.new(|_| {
            SliderState::new()
                .min(min)
                .max(max)
                .step(step)
                .default_value(value)
        });
        Self {
            state,
            value,
            save: None,
        }
    }

    /// The new value if the slider moved, written with `set` once it rests;
    /// a newer change replaces (and so cancels) the pending write.
    fn changed(&mut self, set: fn(&mut Config, f32), cx: &mut Context<PillPreview>) -> Option<f32> {
        let value = self.state.read(cx).value().end();
        if (value - self.value).abs() <= f32::EPSILON {
            return None;
        }
        self.value = value;
        let saved = (value * 100.).round() / 100.;
        self.save = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SAVE_DELAY).await;
            let _ = this.update(cx, |_, cx| edit_config(cx, |c| set(c, saved)));
        }));
        cx.notify();
        Some(value)
    }
}

impl Render for PillPreview {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let shape = if pill::holds_still(window, cx) {
            self.demo.settle();
            Shape::still(self.style)
        } else {
            match self.demo.frame(Instant::now()) {
                Some(shape) => {
                    self.schedule_frame(cx);
                    shape
                }
                None => Shape::still(self.style),
            }
        };
        let theme = cx.theme();
        let accent = accent::get(cx).unwrap_or(theme.primary);
        let look = Look::themed(theme, accent, self.opacity.value, self.border.value);
        let (base, check) = (theme.background, theme.muted_foreground.opacity(0.3));

        canvas(
            |_, _, _| {},
            move |bounds, (), window, _| {
                window.with_content_mask(Some(ContentMask { bounds }), |window| {
                    paint_checker(bounds, base, check, window);
                    let pill = Bounds::new(
                        point(
                            bounds.left() + px(CARD_MARGIN),
                            bounds.top() + px(CARD_MARGIN_Y),
                        ),
                        size(px(PILL_WIDTH), px(PILL_HEIGHT)),
                    );
                    pill::paint(pill, &shape, look, SCALE, window);
                });
            },
        )
        .w(px(CARD_WIDTH))
        .h(px(CARD_HEIGHT))
    }
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn paint_checker(bounds: Bounds<Pixels>, base: Hsla, check: Hsla, window: &mut Window) {
    window.paint_quad(fill(bounds, base));
    let (left, top) = (f32::from(bounds.left()), f32::from(bounds.top()));
    let (columns, rows) = (
        (CARD_WIDTH / CHECKER).ceil() as u8,
        (CARD_HEIGHT / CHECKER).ceil() as u8,
    );
    for row in 0..rows {
        for column in (row % 2..columns).step_by(2) {
            let origin = point(
                px(left + f32::from(column) * CHECKER),
                px(top + f32::from(row) * CHECKER),
            );
            window.paint_quad(fill(
                Bounds::new(origin, size(px(CHECKER), px(CHECKER))),
                check,
            ));
        }
    }
}

/// The preview's motion: a synthetic voice speaks while the motion slider
/// or the style changes, then the pill settles the way the overlay's does
/// when recording stops.
struct Demo {
    motion: Motion,
    clock: Clock,
    /// When a setting last changed; `None` once the pill has settled.
    moved_at: Option<Instant>,
    /// Seconds of synthetic voice so far, so each nudge continues the
    /// sentence instead of restarting it.
    spoken: f32,
}

impl Demo {
    fn new(style: OverlayStyle, intensity: f32) -> Self {
        Self {
            motion: Motion::new(style, intensity),
            clock: Clock::default(),
            moved_at: None,
            spoken: 0.,
        }
    }

    fn nudge(&mut self, intensity: f32, now: Instant) {
        self.motion.set_intensity(intensity);
        self.moved_at = Some(now);
    }

    fn settle(&mut self) {
        self.moved_at = None;
        self.clock = Clock::default();
    }

    /// The pill at `now`, or `None` when it is still.
    fn frame(&mut self, now: Instant) -> Option<Shape> {
        let moved_at = self.moved_at?;
        let dt = self.clock.tick(now);
        let speaking = now.saturating_duration_since(moved_at) < HOLD;
        let voice = speaking.then(|| {
            self.spoken += dt;
            synthetic_voice(self.spoken)
        });
        let shape = self.motion.step(dt, voice.as_ref());
        if !speaking && shape.reach() < SETTLED {
            self.settle();
            return None;
        }
        Some(shape)
    }
}

/// A mic level (0..1 over -60..0 dBFS) that sounds like steady talking:
/// syllables rising well into the voice range with room noise between them,
/// some stressed more than others.
fn synthetic_level(t: f32) -> f32 {
    let syllable = (TAU * SYLLABLE_RATE * t).sin().max(0.).sqrt();
    let stress = 0.8 + 0.2 * (TAU * 0.7 * t).sin();
    0.3 + 0.5 * syllable * stress
}

/// That talking as speech bands: a low fundamental and two formants that
/// drift from vowel to vowel, all following the syllables.
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
    use super::*;

    /// `frames` frames at 60 Hz from `start`, nudging the slider to
    /// `intensity` on each of the first `nudging`.
    fn run(
        demo: &mut Demo,
        start: Instant,
        frames: u32,
        nudging: u32,
        intensity: f32,
    ) -> Vec<Option<Shape>> {
        (0..frames)
            .map(|i| {
                let now = start + Duration::from_secs(1) * i / 60;
                if i < nudging {
                    demo.nudge(intensity, now);
                }
                demo.frame(now)
            })
            .collect()
    }

    #[test]
    fn preview_moves_while_the_slider_moves_then_settles() {
        let styles = [
            (OverlayStyle::Wave, 0.4..=1.8),
            (OverlayStyle::Bars, 0.1..=0.4),
        ];
        for (style, settles_within) in styles {
            let start = Instant::now();
            let mut demo = Demo::new(style, 1.);
            assert!(demo.frame(start).is_none(), "idle preview is still");

            let frames = run(&mut demo, start, 200, 60, 1.5);
            let reach = frames[..60]
                .iter()
                .map(|f| f.map_or(0., |s| s.reach()))
                .fold(0., f32::max);
            assert!(reach > 5., "{style:?} moves while the slider does: {reach}");
            let settled_at = frames.iter().position(Option::is_none).unwrap();
            #[allow(clippy::cast_precision_loss)]
            let after_last_change = (settled_at - 60) as f32 / 60.;
            assert!(
                settles_within.contains(&after_last_change),
                "{style:?} eases out after the slider stops: {after_last_change} s"
            );
            assert!(frames[settled_at..].iter().all(Option::is_none));
        }
    }

    #[test]
    fn preview_at_zero_motion_stays_still() {
        for style in [OverlayStyle::Bars, OverlayStyle::Wave] {
            let mut demo = Demo::new(style, 0.);
            let frames = run(&mut demo, Instant::now(), 60, 60, 0.);
            assert!(frames.iter().flatten().all(|s| s.reach() < 1e-6));
        }
    }

    #[test]
    fn synthetic_voice_falls_silent_between_syllables() {
        let pause = 0.75 / SYLLABLE_RATE;
        assert!(synthetic_voice(pause).bands.iter().all(|&b| b < 1e-3));
        let peak = 0.25 / SYLLABLE_RATE;
        assert!(synthetic_voice(peak).bands.iter().any(|&b| b > 0.6));
    }
}
