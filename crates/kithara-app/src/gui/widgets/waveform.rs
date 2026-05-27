use iced::{
    Color, Element, Event, Length, Point, Rectangle, Renderer, Theme,
    mouse::{self, Button, Cursor},
    widget::canvas::{self, Action, Canvas, Frame, Geometry, Path, Stroke, gradient},
};
use kithara::audio::Envelope;

use crate::{gui::message::Message, theme::gui::GuiPalette};

/// Deck waveform: mirrored envelopes filled with played / unplayed
/// vertical gradients split at the playhead, a faint full-height 1/8
/// column grid, a 16-tick beat grid and a playhead. Click or drag the
/// pointer to seek when `duration` is known.
struct Waveform {
    samples: Envelope,
    progress: f32,
    duration: f64,
    p: GuiPalette,
}

/// Maps an absolute cursor x to a seek target in seconds, clamped to the
/// widget width and `[0, duration]`.
fn seek_target(cursor_x: f32, bounds: Rectangle, duration: f64) -> f64 {
    let frac = ((cursor_x - bounds.x) / bounds.width).clamp(0.0, 1.0);
    f64::from(frac) * duration
}

/// Top stop of the unplayed envelope gradient: a desaturated blue with no
/// palette equivalent, kept local as a deliberate decorative tone. The
/// bottom stop reuses the `line` palette token.
const UNPLAYED_TOP: Color = Color::from_rgb(108.0 / 255.0, 111.0 / 255.0, 154.0 / 255.0);

/// Mirrored closed envelope polygon for `samples`, laid out left-to-right
/// from `x0` with horizontal `step` between points. The played/unplayed
/// split uses two disjoint sub-range paths, not `Frame::with_clip`: clipping
/// a gradient canvas mesh corrupts it in `iced_wgpu` (fan artifacts).
fn envelope_path(samples: &[f32], x0: f32, step: f32, h: f32) -> Path {
    let mid = h / 2.0;
    let amp = mid - 4.0;
    Path::new(|b| {
        let mut x = x0;
        for (i, &v) in samples.iter().enumerate() {
            let y = mid - v * amp;
            if i == 0 {
                b.move_to(Point::new(x, y));
            } else {
                b.line_to(Point::new(x, y));
            }
            x += step;
        }
        for &v in samples.iter().rev() {
            x -= step;
            let y = mid + v * amp * 0.85;
            b.line_to(Point::new(x, y));
        }
        b.close();
    })
}

/// Tracks an in-progress pointer drag so cursor moves between press and
/// release keep emitting seek updates even past the widget edges.
#[derive(Default)]
struct DragState {
    dragging: bool,
}

impl canvas::Program<Message> for Waveform {
    type State = DragState;

    fn update(
        &self,
        state: &mut DragState,
        event: &Event,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> Option<Action<Message>> {
        if self.duration <= 0.0 {
            return None;
        }
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(Button::Left)) => {
                cursor.position_over(bounds).map(|pos| {
                    state.dragging = true;
                    Action::publish(Message::SeekChanged(seek_target(
                        pos.x,
                        bounds,
                        self.duration,
                    )))
                    .and_capture()
                })
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) if state.dragging => {
                cursor.position().map(|pos| {
                    Action::publish(Message::SeekChanged(seek_target(
                        pos.x,
                        bounds,
                        self.duration,
                    )))
                    .and_capture()
                })
            }
            Event::Mouse(mouse::Event::ButtonReleased(Button::Left)) if state.dragging => {
                state.dragging = false;
                Some(Action::publish(Message::SeekReleased).and_capture())
            }
            _ => None,
        }
    }

    fn mouse_interaction(
        &self,
        state: &DragState,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> mouse::Interaction {
        if self.duration > 0.0 && (state.dragging || cursor.is_over(bounds)) {
            mouse::Interaction::Pointer
        } else {
            mouse::Interaction::default()
        }
    }

    fn draw(
        &self,
        _state: &DragState,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let w = bounds.width;
        let h = bounds.height;
        if w <= 0.0 || h <= 0.0 {
            return vec![frame.into_geometry()];
        }

        let grid_color = Color {
            a: 0.08,
            ..self.p.accent
        };
        for i in 1u16..=8 {
            let x = (f32::from(i) / 8.0) * w;
            frame.stroke(
                &Path::line(Point::new(x, 0.0), Point::new(x, h)),
                Stroke::default().with_color(grid_color).with_width(1.0),
            );
        }

        let n = self.samples.len();
        if n >= 2 {
            #[expect(
                clippy::cast_precision_loss,
                reason = "envelope point count tracks display width and never nears the f32 mantissa limit"
            )]
            let last = (n - 1) as f32;
            let step = w / last;

            let unplayed = gradient::Linear::new(Point::new(0.0, 0.0), Point::new(0.0, h))
                .add_stop(
                    0.0,
                    Color {
                        a: 0.55,
                        ..UNPLAYED_TOP
                    },
                )
                .add_stop(
                    1.0,
                    Color {
                        a: 0.65,
                        ..self.p.line
                    },
                );
            let played = gradient::Linear::new(Point::new(0.0, 0.0), Point::new(0.0, h))
                .add_stop(
                    0.0,
                    Color {
                        a: 0.95,
                        ..self.p.accent_strong
                    },
                )
                .add_stop(
                    1.0,
                    Color {
                        a: 0.70,
                        ..self.p.accent
                    },
                );

            // Sample index at the playhead; both sub-paths share this boundary.
            // x0 comes from the pre-truncation float so the slice cast is the
            // only one needed.
            let split_f = (self.progress.clamp(0.0, 1.0) * last).round();
            let x0 = split_f * step;
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "split_f is rounded and in [0, last]; in range for a slice index"
            )]
            let split = split_f as usize;

            if split >= 1 {
                let path = envelope_path(&self.samples[..=split], 0.0, step, h);
                frame.fill(&path, played);
            }
            if split <= n - 2 {
                let path = envelope_path(&self.samples[split..], x0, step, h);
                frame.fill(&path, unplayed);
            }
        }

        let tick_color = Color {
            a: 0.4,
            ..self.p.accent
        };
        for i in 0u16..16 {
            let x = (f32::from(i) / 16.0) * w;
            frame.stroke(
                &Path::line(Point::new(x, h - 6.0), Point::new(x, h)),
                Stroke::default().with_color(tick_color).with_width(1.0),
            );
        }

        let px = self.progress.clamp(0.0, 1.0) * w;
        let head = Path::line(Point::new(px, 0.0), Point::new(px, h));
        frame.stroke(
            &head,
            Stroke::default().with_color(self.p.accent).with_width(2.0),
        );

        vec![frame.into_geometry()]
    }
}

/// Build the deck waveform element from a precomputed peak envelope.
/// `duration` (track length in seconds) enables click/drag seeking; pass
/// `0.0` when it is unknown to keep the widget display-only.
pub(crate) fn waveform<'a>(
    peaks: Envelope,
    progress: f32,
    duration: f64,
    height: f32,
    p: GuiPalette,
) -> Element<'a, Message> {
    Canvas::new(Waveform {
        samples: peaks,
        progress,
        duration,
        p,
    })
    .width(Length::Fill)
    .height(Length::Fixed(height))
    .into()
}
