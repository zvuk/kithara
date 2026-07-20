use iced::{
    Color, Element, Length, Point, Rectangle, Renderer, Size, Theme,
    mouse::{self, Cursor},
    widget::canvas::{self, Action, Canvas, Frame, Geometry, Path, Stroke, gradient},
};

use crate::{
    gui::{dj::DjMsg, message::Message},
    theme::gui::GuiPalette,
};

const HANDLE: f32 = 18.0;

/// Horizontal timestretch slider: the gold fill spans from the centre toward
/// the handle, snaps to 0 within `range * 0.012`, and rounds to 0.01. Five
/// ticks with a taller centre tick; an 18x18 glowing square handle.
struct TsSlider {
    p: GuiPalette,
    range: f32,
    tempo: f32,
}

impl TsSlider {
    fn to_x(&self, w: f32, v: f32) -> f32 {
        let pad = HANDLE / 2.0;
        let usable = (w - HANDLE).max(1.0);
        pad + (((v + self.range) / (2.0 * self.range)).clamp(0.0, 1.0)) * usable
    }

    fn value_at(&self, bounds: Rectangle, pos: Point) -> f32 {
        let pad = HANDLE / 2.0;
        let usable = (bounds.width - HANDLE).max(1.0);
        let x = ((pos.x - bounds.x - pad) / usable).clamp(0.0, 1.0);
        let mut v = -self.range + x * 2.0 * self.range;
        if v.abs() < self.range * 0.012 {
            v = 0.0;
        }
        (v * 100.0).round() / 100.0
    }
}

impl canvas::Program<Message> for TsSlider {
    type State = bool;

    fn draw(
        &self,
        _state: &bool,
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
        let mid_y = h / 2.0;

        frame.stroke(
            &Path::line(
                Point::new(HANDLE / 2.0, mid_y),
                Point::new(w - HANDLE / 2.0, mid_y),
            ),
            Stroke::default()
                .with_color(Color {
                    a: 0.4,
                    ..self.p.line
                })
                .with_width(3.0),
        );

        let center_x = self.to_x(w, 0.0);
        let val_x = self.to_x(w, self.tempo.clamp(-self.range, self.range));
        frame.stroke(
            &Path::line(Point::new(center_x, mid_y), Point::new(val_x, mid_y)),
            Stroke::default().with_color(self.p.accent).with_width(3.0),
        );

        let fill_lo = center_x.min(val_x);
        let fill_hi = center_x.max(val_x);
        for (i, t) in [0.0_f32, 0.25, 0.5, 0.75, 1.0].into_iter().enumerate() {
            let x = HANDLE / 2.0 + t * (w - HANDLE);
            let center = i == 2;
            let th = if center { 8.0 } else { 5.0 };
            let covered = x >= fill_lo - 0.5 && x <= fill_hi + 0.5;
            frame.stroke(
                &Path::line(Point::new(x, mid_y - th), Point::new(x, mid_y + th)),
                Stroke::default()
                    .with_color(if covered { self.p.accent } else { self.p.line })
                    .with_width(1.0),
            );
        }

        // Soft glow under the handle (2 translucent rounded passes).
        for &(grow, alpha) in &[(2.5, 0.16), (1.2, 0.28)] {
            frame.fill(
                &Path::rounded_rectangle(
                    Point::new(val_x - HANDLE / 2.0 - grow, mid_y - HANDLE / 2.0 - grow),
                    Size::new(HANDLE + grow * 2.0, HANDLE + grow * 2.0),
                    4.0.into(),
                ),
                Color {
                    a: alpha,
                    ..self.p.accent_glow
                },
            );
        }

        let handle = Path::rounded_rectangle(
            Point::new(val_x - HANDLE / 2.0, mid_y - HANDLE / 2.0),
            Size::new(HANDLE, HANDLE),
            4.0.into(),
        );
        frame.fill(
            &handle,
            gradient::Linear::new(
                Point::new(val_x, mid_y - HANDLE / 2.0),
                Point::new(val_x, mid_y + HANDLE / 2.0),
            )
            .add_stop(0.0, self.p.accent_strong)
            .add_stop(1.0, self.p.accent),
        );
        frame.stroke(
            &handle,
            Stroke::default()
                .with_color(Color {
                    a: 0.6,
                    ..self.p.bg
                })
                .with_width(1.0),
        );

        vec![frame.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        _state: &bool,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> mouse::Interaction {
        if cursor.is_over(bounds) {
            mouse::Interaction::Grab
        } else {
            mouse::Interaction::default()
        }
    }

    fn update(
        &self,
        dragging: &mut bool,
        event: &canvas::Event,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> Option<Action<Message>> {
        match event {
            canvas::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let pos = cursor.position_in(bounds)?;
                *dragging = true;
                let abs = Point::new(bounds.x + pos.x, bounds.y + pos.y);
                Some(
                    Action::publish(Message::Dj(DjMsg::SetTempo(self.value_at(bounds, abs))))
                        .and_capture(),
                )
            }
            canvas::Event::Mouse(mouse::Event::CursorMoved { .. }) if *dragging => {
                let pos = cursor.position()?;
                Some(
                    Action::publish(Message::Dj(DjMsg::SetTempo(self.value_at(bounds, pos))))
                        .and_capture(),
                )
            }
            canvas::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
                if *dragging =>
            {
                *dragging = false;
                Some(Action::capture())
            }
            _ => None,
        }
    }
}

/// Build the interactive timestretch slider for the DJ deck.
pub(crate) fn ts_slider<'a>(tempo: f32, range: f32, p: GuiPalette) -> Element<'a, Message> {
    Canvas::new(TsSlider { p, range, tempo })
        .width(Length::Fill)
        .height(Length::Fixed(30.0))
        .into()
}
