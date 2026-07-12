use iced::{
    Color, Element, Event, Length, Point, Rectangle, Renderer, Size, Theme,
    mouse::{self, Button, Cursor, ScrollDelta},
    widget::canvas::{self, Action, Canvas, Frame, Geometry, Path, Stroke},
};
use kithara::audio::Waveform;
use kithara_platform::sync::Arc;
use num_traits::cast::AsPrimitive;

use super::waveform_viewport::{Viewport, WaveMsg};
use crate::{
    gui::{dj::DjMsg, message::Message},
    theme::gui::{GuiPalette, WAVE_HIGH, WAVE_LOW, WAVE_MID},
};

/// Beat-grid overlay data for the deck waveform: positions as track fractions
/// in `[0, 1]`, derived from the track's analysed beat grid.
pub(crate) struct BeatMarks {
    pub(crate) beats: Arc<[f32]>,
    pub(crate) downbeats: Arc<[f32]>,
}

/// Deck waveform: three concentric mirrored band bars over a grid + playhead.
struct WaveformCanvas {
    beats: Arc<[f32]>,
    downbeats: Arc<[f32]>,
    p: GuiPalette,
    view: Viewport,
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

fn wheel_factor(delta: ScrollDelta) -> f32 {
    const WHEEL_BASE: f32 = 1.2;
    const WHEEL_PIXELS_PER_LINE: f32 = 50.0;

    let lines = match delta {
        ScrollDelta::Lines { y, .. } => y,
        ScrollDelta::Pixels { y, .. } => y / WHEEL_PIXELS_PER_LINE,
    };
    WHEEL_BASE.powf(lines)
}

#[derive(Clone, Copy)]
struct Press {
    moved: bool,
    last_x: f32,
    start_x: f32,
}

#[derive(Default)]
struct PointerState {
    press: Option<Press>,
}

impl WaveformCanvas {
    fn draw_beatgrid(&self, frame: &mut Frame, w: f32, h: f32) {
        self.draw_beats(frame, w, h);
        self.draw_downbeats(frame, w, h);
    }

    fn draw_beats(&self, frame: &mut Frame, w: f32, h: f32) {
        const BEAT_MIN_GAP_PX: f32 = 5.0;

        if self.beats.is_empty() {
            return;
        }
        let beat_color = Color {
            a: 0.18,
            ..self.p.accent
        };
        let mut last_x = f32::NEG_INFINITY;
        for &frac in self.beats.iter() {
            let x = self.view.screen_frac(frac) * w;
            if !(0.0..=w).contains(&x) || (x - last_x).abs() < BEAT_MIN_GAP_PX {
                continue;
            }
            last_x = x;
            frame.stroke(
                &Path::line(Point::new(x, 0.0), Point::new(x, h)),
                Stroke::default().with_color(beat_color).with_width(1.0),
            );
        }
    }

    fn draw_downbeats(&self, frame: &mut Frame, w: f32, h: f32) {
        const DOWNBEAT_MIN_GAP_PX: f32 = 6.0;

        let downbeats = &self.downbeats;
        let len = downbeats.len();
        if len == 0 {
            return;
        }

        let accent = self.p.accent;
        let accent_strong = self.p.accent_strong;
        let active = downbeats.iter().rposition(|&d| d <= self.progress + 1e-4);

        let mut last_x = f32::NEG_INFINITY;
        for i in 0..len {
            let frac = downbeats[i];
            let x = self.view.screen_frac(frac) * w;
            let is_active = active == Some(i);

            if is_active {
                let end = if i + 1 < len { downbeats[i + 1] } else { 1.0 };
                let x_end = self.view.screen_frac(end) * w;
                let cx0 = x.clamp(0.0, w);
                let cx1 = x_end.clamp(0.0, w);
                let seg_w = (cx1 - cx0).max(0.0);
                frame.fill_rectangle(
                    Point::new(cx0, 0.0),
                    Size::new(seg_w, h),
                    Color { a: 0.12, ..accent },
                );
            }

            if !(0.0..=w).contains(&x) {
                continue;
            }

            if !is_active && (x - last_x).abs() < DOWNBEAT_MIN_GAP_PX {
                continue;
            }
            last_x = x;

            let (marker_color, marker_w) = if is_active {
                (accent_strong, 2.0)
            } else {
                (Color { a: 0.6, ..accent }, 1.0)
            };
            frame.stroke(
                &Path::line(Point::new(x, 0.0), Point::new(x, h)),
                Stroke::default()
                    .with_color(marker_color)
                    .with_width(marker_w),
            );
            let tab = Path::new(|b| {
                b.move_to(Point::new(x - 4.0, 0.0));
                b.line_to(Point::new(x + 4.0, 0.0));
                b.line_to(Point::new(x, 6.0));
                b.close();
            });
            frame.fill(&tab, accent_strong);
        }
    }
}

impl canvas::Program<Message> for WaveformCanvas {
    type State = PointerState;

    fn draw(
        &self,
        _state: &PointerState,
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
            let x = self.view.screen_frac(f32::from(i) / 8.0) * w;
            if (0.0..=w).contains(&x) {
                frame.stroke(
                    &Path::line(Point::new(x, 0.0), Point::new(x, h)),
                    Stroke::default().with_color(grid_color).with_width(1.0),
                );
            }
        }

        let head_x = self.view.screen_frac(self.progress.clamp(0.0, 1.0)) * w;

        let buckets = self.wave.buckets();
        let n = buckets.len();
        if n >= 1 {
            let mid = h / 2.0;
            let amp = (mid - 4.0).max(0.0);
            let bands = [WAVE_LOW, WAVE_MID, WAVE_HIGH];

            let cols: usize = w.ceil().as_();
            for col in 0..cols {
                let x: f32 = col.as_();
                let (lo, hi) = self.view.pixel_buckets(x, w, n);
                let mut peak = [0.0_f32; 3];
                for b in &buckets[lo..hi] {
                    peak[0] = peak[0].max(b.low);
                    peak[1] = peak[1].max(b.mid);
                    peak[2] = peak[2].max(b.high);
                }
                let played = (x + 0.5) <= head_x;
                for (value, base) in peak.iter().zip(bands.iter()) {
                    let v = value.clamp(0.0, 1.0);
                    if v <= 0.0 {
                        continue;
                    }
                    let half = v * amp;
                    let color = if played { dim(*base) } else { *base };
                    frame.fill_rectangle(
                        Point::new(x, mid - half),
                        Size::new(1.0, half * 2.0),
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
            let x = self.view.screen_frac(f32::from(i) / 16.0) * w;
            if (0.0..=w).contains(&x) {
                frame.stroke(
                    &Path::line(Point::new(x, h - 6.0), Point::new(x, h)),
                    Stroke::default().with_color(tick_color).with_width(1.0),
                );
            }
        }

        self.draw_beatgrid(&mut frame, w, h);

        if (0.0..=w).contains(&head_x) {
            frame.stroke(
                &Path::line(Point::new(head_x, 0.0), Point::new(head_x, h)),
                Stroke::default().with_color(self.p.accent).with_width(2.0),
            );
        }

        vec![frame.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        state: &PointerState,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> mouse::Interaction {
        if state.press.is_some() {
            mouse::Interaction::Grabbing
        } else if cursor.is_over(bounds) {
            mouse::Interaction::Grab
        } else {
            mouse::Interaction::default()
        }
    }

    fn update(
        &self,
        state: &mut PointerState,
        event: &Event,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> Option<Action<Message>> {
        const DRAG_THRESHOLD_PX: f32 = 3.0;

        match event {
            Event::Mouse(mouse::Event::WheelScrolled { delta }) if cursor.is_over(bounds) => {
                let factor = wheel_factor(*delta);
                let cursor_x = cursor.position().map_or(bounds.x, |p| p.x);
                let anchor = ((cursor_x - bounds.x) / bounds.width).clamp(0.0, 1.0);
                ((factor - 1.0).abs() >= f32::EPSILON)
                    .then(|| zoom_action(WaveMsg::ZoomBy { factor, anchor }))
            }
            Event::Mouse(mouse::Event::ButtonPressed(Button::Left)) => {
                cursor.position_over(bounds).map(|pos| {
                    state.press = Some(Press {
                        start_x: pos.x,
                        last_x: pos.x,
                        moved: false,
                    });
                    Action::capture()
                })
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                let press = state.press.as_mut()?;
                let pos = cursor.position()?;
                let dx = pos.x - press.last_x;
                press.last_x = pos.x;
                if (pos.x - press.start_x).abs() > DRAG_THRESHOLD_PX {
                    press.moved = true;
                }
                if press.moved {
                    Some(zoom_action(WaveMsg::Pan(dx / bounds.width)))
                } else {
                    Some(Action::capture())
                }
            }
            Event::Mouse(mouse::Event::ButtonReleased(Button::Left)) => {
                let press = state.press.take()?;
                if press.moved || self.duration <= 0.0 {
                    return Some(Action::capture());
                }
                let x = cursor.position().map_or(press.last_x, |p| p.x);
                let frac = ((x - bounds.x) / bounds.width).clamp(0.0, 1.0);
                let secs = (f64::from(self.view.track_frac(frac)) * self.duration)
                    .clamp(0.0, self.duration);
                Some(Action::publish(Message::SeekTo(secs)).and_capture())
            }
            _ => None,
        }
    }
}

fn zoom_action(msg: WaveMsg) -> Action<Message> {
    Action::publish(Message::Dj(DjMsg::Wave(msg))).and_capture()
}

/// Deck waveform element. `view` is the zoom/pan window.
pub(crate) fn waveform<'a>(
    wave: Waveform,
    marks: BeatMarks,
    progress: f32,
    duration: f64,
    view: Viewport,
    height: f32,
    p: GuiPalette,
) -> Element<'a, Message> {
    Canvas::new(WaveformCanvas {
        p,
        wave,
        progress,
        duration,
        view,
        beats: marks.beats,
        downbeats: marks.downbeats,
    })
    .width(Length::Fill)
    .height(Length::Fixed(height))
    .into()
}
