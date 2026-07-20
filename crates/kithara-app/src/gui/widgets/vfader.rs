use iced::{
    Color, Element, Event, Length, Point, Rectangle, Renderer, Size, Theme,
    mouse::{self, Button, Cursor},
    widget::canvas::{self, Action, Canvas, Frame, Geometry, Path, Stroke, gradient},
};

use crate::{gui::message::Message, theme::gui::GuiPalette};

/// Vertical EQ fader: a thin rail with a zero line, a gradient fill growing
/// from 0 dB toward the current value and a round white handle with an accent
/// border. Click or drag the rail to set the band gain; snaps to 0 within
/// 0.3 dB and rounds to one decimal.
struct VFader {
    p: GuiPalette,
    max: f32,
    min: f32,
    value: f32,
    band: usize,
}

mod consts {
    pub(super) const RAIL_W: f32 = 5.0;
    pub(super) const HANDLE: f32 = 13.0;
}
use consts::*;

/// Gain at the pointer for a fader of pixel `height` and the cursor at
/// `local_y` (relative to the widget top). Top of the rail is `max`, bottom
/// is `min`; the result snaps to 0 within 0.3 dB and rounds to one decimal.
fn value_at(min: f32, max: f32, height: f32, local_y: f32) -> f32 {
    let top = HANDLE / 2.0;
    let usable = (height - HANDLE).max(1.0);
    let frac = (1.0 - (local_y - top) / usable).clamp(0.0, 1.0);
    let mut v = min + frac * (max - min);
    if v.abs() < 0.3 {
        v = 0.0;
    }
    (v * 10.0).round() / 10.0
}

/// Pixel `y` of value `v` for a rail of `height`, matching `value_at`.
fn y_of(min: f32, max: f32, height: f32, v: f32) -> f32 {
    let top = HANDLE / 2.0;
    let usable = (height - HANDLE).max(1.0);
    let span = (max - min).max(f32::EPSILON);
    top + (1.0 - (v - min) / span) * usable
}

/// Tracks an in-progress drag so cursor moves past the rail edges keep
/// emitting gain updates until the button is released.
#[derive(Default)]
struct DragState {
    dragging: bool,
}

impl canvas::Program<Message> for VFader {
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
        let cx = w / 2.0;
        let top = HANDLE / 2.0;
        let usable = (h - HANDLE).max(1.0);

        frame.fill(
            &Path::rounded_rectangle(
                Point::new(cx - RAIL_W / 2.0, top),
                Size::new(RAIL_W, usable),
                2.0.into(),
            ),
            Color {
                a: 0.35,
                ..self.p.muted
            },
        );

        let zero_y = y_of(self.min, self.max, h, 0.0);
        let val_y = y_of(self.min, self.max, h, self.value.clamp(self.min, self.max));
        let (fy, fh) = if val_y < zero_y {
            (val_y, zero_y - val_y)
        } else {
            (zero_y, val_y - zero_y)
        };
        // At 0 dB the fill collapses to nothing; skip it so the rail reads clean.
        if fh > 0.5 {
            let fill = gradient::Linear::new(Point::new(0.0, fy), Point::new(0.0, fy + fh))
                .add_stop(0.0, self.p.accent_strong)
                .add_stop(1.0, self.p.accent);
            frame.fill(
                &Path::rounded_rectangle(
                    Point::new(cx - RAIL_W / 2.0, fy),
                    Size::new(RAIL_W, fh),
                    2.0.into(),
                ),
                fill,
            );
        }

        frame.stroke(
            &Path::line(
                Point::new(cx - RAIL_W, zero_y),
                Point::new(cx + RAIL_W, zero_y),
            ),
            Stroke::default()
                .with_color(Color {
                    a: 0.6,
                    ..self.p.muted
                })
                .with_width(1.0),
        );

        let handle = Path::circle(Point::new(cx, val_y), HANDLE / 2.0);
        frame.fill(&handle, Color::WHITE);
        frame.stroke(
            &handle,
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
        if state.dragging || cursor.is_over(bounds) {
            mouse::Interaction::Grab
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
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(Button::Left)) => {
                cursor.position_over(bounds).map(|pos| {
                    state.dragging = true;
                    let v = value_at(self.min, self.max, bounds.height, pos.y - bounds.y);
                    Action::publish(Message::EqBandChanged(self.band, v)).and_capture()
                })
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) if state.dragging => {
                cursor.position().map(|pos| {
                    let v = value_at(self.min, self.max, bounds.height, pos.y - bounds.y);
                    Action::publish(Message::EqBandChanged(self.band, v)).and_capture()
                })
            }
            Event::Mouse(mouse::Event::ButtonReleased(Button::Left)) if state.dragging => {
                state.dragging = false;
                Some(Action::capture())
            }
            _ => None,
        }
    }
}

/// Range, current value, and pixel height for a single [`vfader`].
#[derive(Clone, Copy)]
pub(crate) struct VFaderParams {
    pub(crate) height: f32,
    pub(crate) max: f32,
    pub(crate) min: f32,
    pub(crate) value: f32,
}

/// Build a vertical EQ fader of pixel `params.height` for `band`, with
/// `params.value` in `params.min..=params.max` dB.
pub(crate) fn vfader<'a>(band: usize, params: VFaderParams, p: GuiPalette) -> Element<'a, Message> {
    let VFaderParams {
        value,
        min,
        max,
        height,
    } = params;
    Canvas::new(VFader {
        p,
        max,
        min,
        value,
        band,
    })
    .width(Length::Fill)
    .height(Length::Fixed(height))
    .into()
}

#[cfg(test)]
mod tests {
    use super::{value_at, y_of};

    mod consts {
        pub(super) const MIN: f32 = -24.0;
        pub(super) const MAX: f32 = 6.0;
        pub(super) const H: f32 = 120.0;
    }
    use consts::*;

    #[test]
    fn top_of_rail_reads_max_gain() {
        assert_eq!(value_at(MIN, MAX, H, super::HANDLE / 2.0), MAX);
    }

    #[test]
    fn bottom_of_rail_reads_min_gain() {
        assert_eq!(value_at(MIN, MAX, H, H - super::HANDLE / 2.0), MIN);
    }

    #[test]
    fn near_zero_snaps_to_zero() {
        let zero_y = y_of(MIN, MAX, H, 0.0);
        assert_eq!(value_at(MIN, MAX, H, zero_y), 0.0);
    }

    #[test]
    fn value_and_y_round_trip() {
        let y = y_of(MIN, MAX, H, -6.0);
        assert!((value_at(MIN, MAX, H, y) - (-6.0)).abs() < 0.2);
    }
}
