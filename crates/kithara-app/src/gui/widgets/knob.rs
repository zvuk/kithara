use iced::{
    Color, Element, Length, Point, Rectangle, Renderer, Theme,
    mouse::{self, Cursor, ScrollDelta},
    widget::canvas::{self, Action, Canvas, Frame, Geometry, Path, Stroke},
};

use crate::{
    gui::{message::Message, view::mix_colors},
    theme::gui::GuiPalette,
};

/// A 270 degree arc dial. Display-only: background arc, glowing accent
/// foreground arc, layered body and an indicator line at the current angle.
struct Knob<F> {
    value: f32,
    min: f32,
    max: f32,
    p: GuiPalette,
    on_change: F,
}

const SWEEP: f32 = 270.0;
const START: f32 = 135.0;

const BODY_CENTER: Color = Color::from_rgb(42.0 / 255.0, 42.0 / 255.0, 68.0 / 255.0);
const BODY_MID: Color = Color::from_rgb(21.0 / 255.0, 21.0 / 255.0, 42.0 / 255.0);
const BODY_EDGE: Color = Color::from_rgb(10.0 / 255.0, 10.0 / 255.0, 24.0 / 255.0);
const WHEEL_STEP: f32 = 0.5;
const PIXELS_PER_STEP: f32 = 40.0;

/// Radial body colour at normalised distance `u` from the centre
/// (`0.0` centre .. `1.0` edge): centre -> mid (55%) -> edge.
fn body_color(u: f32) -> Color {
    if u <= 0.55 {
        mix_colors(BODY_CENTER, BODY_MID, u / 0.55)
    } else {
        mix_colors(BODY_MID, BODY_EDGE, (u - 0.55) / 0.45)
    }
}
const ARC_BG: Color = Color {
    r: 40.0 / 255.0,
    g: 40.0 / 255.0,
    b: 72.0 / 255.0,
    a: 0.9,
};

fn polar(cx: f32, cy: f32, angle_deg: f32, rad: f32) -> Point {
    let t = (angle_deg + 90.0).to_radians();
    Point::new(cx + rad * t.cos(), cy + rad * t.sin())
}

fn arc_path(cx: f32, cy: f32, r: f32, a0: f32, a1: f32) -> Path {
    Path::new(|b| {
        let segments: u16 = 64;
        for i in 0..=segments {
            let a = a0 + (a1 - a0) * (f32::from(i) / f32::from(segments));
            let pt = polar(cx, cy, a, r);
            if i == 0 {
                b.move_to(pt);
            } else {
                b.line_to(pt);
            }
        }
    })
}

fn round_knob_value(value: f32) -> f32 {
    let rounded = (value * 10.0).round() / 10.0;
    if rounded.abs() < 0.05 { 0.0 } else { rounded }
}

fn wheel_delta_units(delta: ScrollDelta) -> f32 {
    match delta {
        ScrollDelta::Lines { y, .. } => y,
        ScrollDelta::Pixels { y, .. } => y / PIXELS_PER_STEP,
    }
}

fn next_value_from_wheel(value: f32, min: f32, max: f32, delta: ScrollDelta) -> Option<f32> {
    let units = wheel_delta_units(delta);
    if units.abs() < f32::EPSILON {
        return None;
    }

    let next = round_knob_value((value + units * WHEEL_STEP).clamp(min, max));
    if (next - value).abs() < f32::EPSILON {
        None
    } else {
        Some(next)
    }
}

impl<F> canvas::Program<Message> for Knob<F>
where
    F: Fn(f32) -> Message,
{
    type State = ();

    fn update(
        &self,
        _state: &mut (),
        event: &canvas::Event,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> Option<Action<Message>> {
        match event {
            canvas::Event::Mouse(mouse::Event::WheelScrolled { delta })
                if cursor.is_over(bounds) =>
            {
                next_value_from_wheel(self.value, self.min, self.max, *delta)
                    .map(|value| Action::publish((self.on_change)(value)).and_capture())
            }
            _ => None,
        }
    }

    fn draw(
        &self,
        _state: &(),
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let sz = bounds.width.min(bounds.height);
        if sz <= 0.0 {
            return vec![frame.into_geometry()];
        }
        let cx = bounds.width / 2.0;
        let cy = bounds.height / 2.0;
        let r = sz * 0.42;
        let body_r = sz * 0.34;
        let pct = if (self.max - self.min).abs() < f32::EPSILON {
            0.0
        } else {
            ((self.value - self.min) / (self.max - self.min)).clamp(0.0, 1.0)
        };
        let end = START + SWEEP * pct;

        frame.stroke(
            &arc_path(cx, cy, r, START, START + SWEEP),
            Stroke::default().with_color(ARC_BG).with_width(3.0),
        );

        let fg = arc_path(cx, cy, r, START, end);
        // Soft glow: graduated wide-to-narrow translucent passes (iced
        // canvas has no blur), then the sharp arc on top.
        for i in 0u16..6 {
            let t = f32::from(i) / 5.0;
            let width = 5.0 - t * 2.0;
            let alpha = 0.06 + t * 0.20;
            frame.stroke(
                &fg,
                Stroke::default()
                    .with_color(Color {
                        a: alpha,
                        ..self.p.accent
                    })
                    .with_width(width),
            );
        }
        frame.stroke(
            &fg,
            Stroke::default().with_color(self.p.accent).with_width(3.0),
        );

        let center = Point::new(cx, cy);
        let steps: u16 = 40;
        for i in 0..steps {
            let frac = f32::from(i) / f32::from(steps);
            let r = body_r * (1.0 - frac);
            if r > 0.0 {
                frame.fill(&Path::circle(center, r), body_color(1.0 - frac));
            }
        }
        frame.stroke(
            &Path::circle(Point::new(cx, cy - body_r * 0.15), body_r * 0.85),
            Stroke::default()
                .with_color(Color {
                    a: 0.05,
                    ..Color::WHITE
                })
                .with_width(1.0),
        );

        let indicator = Path::line(
            polar(cx, cy, end, body_r * 0.35),
            polar(cx, cy, end, body_r + 1.0),
        );
        frame.stroke(
            &indicator,
            Stroke::default()
                .with_color(Color {
                    a: 0.22,
                    ..self.p.accent
                })
                .with_width(3.2),
        );
        frame.stroke(
            &indicator,
            Stroke::default().with_color(self.p.accent).with_width(2.5),
        );

        vec![frame.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        _state: &(),
        bounds: Rectangle,
        cursor: Cursor,
    ) -> mouse::Interaction {
        if cursor.is_over(bounds) {
            mouse::Interaction::Grab
        } else {
            mouse::Interaction::default()
        }
    }
}

/// Build a knob canvas of the given pixel size for `value` in `min..=max`.
pub(crate) fn knob<'a, F>(
    value: f32,
    min: f32,
    max: f32,
    size: f32,
    p: GuiPalette,
    on_change: F,
) -> Element<'a, Message>
where
    F: 'a + Fn(f32) -> Message,
{
    Canvas::new(Knob {
        value,
        min,
        max,
        p,
        on_change,
    })
    .width(Length::Fixed(size))
    .height(Length::Fixed(size))
    .into()
}

#[cfg(test)]
mod tests {
    use iced::mouse::ScrollDelta;

    use super::next_value_from_wheel;

    #[test]
    fn line_scroll_increases_value_by_half_db() {
        let next = next_value_from_wheel(0.0, -12.0, 12.0, ScrollDelta::Lines { x: 0.0, y: 1.0 });

        assert_eq!(next, Some(0.5));
    }

    #[test]
    fn pixel_scroll_is_normalized_and_clamped() {
        let next =
            next_value_from_wheel(11.8, -12.0, 12.0, ScrollDelta::Pixels { x: 0.0, y: 40.0 });

        assert_eq!(next, Some(12.0));
    }

    #[test]
    fn zero_scroll_does_not_emit_update() {
        let next = next_value_from_wheel(2.0, -12.0, 12.0, ScrollDelta::Lines { x: 0.0, y: 0.0 });

        assert_eq!(next, None);
    }
}
