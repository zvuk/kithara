use iced::{
    Color, Element, Event, Length, Point, Rectangle, Renderer, Size, Theme,
    mouse::{self, Button, Cursor},
    widget::canvas::{self, Action, Canvas, Frame, Geometry, Path, Stroke},
};
use kithara::audio::Waveform;
use num_traits::cast::AsPrimitive;

use crate::{
    gui::message::Message,
    theme::gui::{GuiPalette, WAVE_HIGH, WAVE_LOW, WAVE_MID},
};

/// Deck waveform: per bucket, three concentric mirrored bars (low/mid/high band
/// heights) painted low->mid->high so bass forms the outer hull and highs sit
/// in the center, over a faint 1/8 column grid, a 16-tick beat grid and a
/// playhead. Click or drag the pointer to seek when `duration` is known.
struct WaveformCanvas {
    p: GuiPalette,
    wave: Waveform,
    progress: f32,
    duration: f64,
}

/// Dim a played-past band color so the playhead split reads at a glance.
fn dim(c: Color) -> Color {
    Color {
        r: c.r * 0.42,
        g: c.g * 0.42,
        b: c.b * 0.42,
        a: 1.0,
    }
}

/// Maps an absolute cursor x to a seek target in seconds, clamped to the
/// widget width and `[0, duration]`.
fn seek_target(cursor_x: f32, bounds: Rectangle, duration: f64) -> f64 {
    let frac = ((cursor_x - bounds.x) / bounds.width).clamp(0.0, 1.0);
    f64::from(frac) * duration
}

/// Tracks an in-progress pointer drag so cursor moves between press and
/// release keep emitting seek updates even past the widget edges.
#[derive(Default)]
struct DragState {
    dragging: bool,
}

impl canvas::Program<Message> for WaveformCanvas {
    type State = DragState;

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

        let buckets = self.wave.buckets();
        let n = buckets.len();
        if n >= 1 {
            let mid = h / 2.0;
            let amp = (mid - 4.0).max(0.0);
            let count: f32 = n.as_();
            let col_w = w / count;
            let bar_w = col_w.max(1.0);
            let head_x = self.progress.clamp(0.0, 1.0) * w;
            let bands = [WAVE_LOW, WAVE_MID, WAVE_HIGH];

            for (i, bucket) in buckets.iter().enumerate() {
                let idx: f32 = i.as_();
                let x = idx * col_w;
                let played = (x + bar_w * 0.5) <= head_x;
                let heights = [bucket.low, bucket.mid, bucket.high];

                for (value, base) in heights.iter().zip(bands.iter()) {
                    let v = value.clamp(0.0, 1.0);
                    if v <= 0.0 {
                        continue;
                    }
                    let half = v * amp;
                    // The played past is dimmed so the upcoming part stays vivid.
                    let color = if played { dim(*base) } else { *base };
                    frame.fill_rectangle(
                        Point::new(x, mid - half),
                        Size::new(bar_w, half * 2.0),
                        color,
                    );
                }
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
}

/// Build the deck waveform element from a precomputed colored waveform.
/// `duration` (track length in seconds) enables click/drag seeking; pass
/// `0.0` when it is unknown to keep the widget display-only.
pub(crate) fn waveform<'a>(
    wave: Waveform,
    progress: f32,
    duration: f64,
    height: f32,
    p: GuiPalette,
) -> Element<'a, Message> {
    Canvas::new(WaveformCanvas {
        p,
        wave,
        progress,
        duration,
    })
    .width(Length::Fill)
    .height(Length::Fixed(height))
    .into()
}
