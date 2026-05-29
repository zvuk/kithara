use iced::{
    Color, Element, Length, Pixels, Point, Rectangle, Renderer, Theme,
    mouse::{self, Cursor},
    widget::canvas::{self, Action, Canvas, Frame, Geometry, Path, Stroke, Text},
};

use crate::{gui::message::Message, theme::gui::GuiPalette};

const SNAPS: [f32; 6] = [0.5, 0.75, 1.0, 1.25, 1.5, 2.0];
const MIN: f32 = 0.5;
const MAX: f32 = 2.0;
const HANDLE: f32 = 16.0;

/// Horizontal speed scrubber: the gold fill spans from 1.0x to the
/// current value, snap targets sit at `SNAPS` (within 0.04) and round
/// to 0.01.
struct SpeedSlider {
    value: f32,
    p: GuiPalette,
}

impl SpeedSlider {
    fn value_at(bounds: Rectangle, pos: Point) -> f32 {
        let pad = HANDLE / 2.0;
        let usable = (bounds.width - HANDLE).max(1.0);
        let x = ((pos.x - bounds.x - pad) / usable).clamp(0.0, 1.0);
        let mut v = MIN + x * (MAX - MIN);
        for s in SNAPS {
            if (v - s).abs() < 0.04 {
                v = s;
                break;
            }
        }
        (v * 100.0).round() / 100.0
    }
}

fn to_x(bounds: Rectangle, v: f32) -> f32 {
    let pad = HANDLE / 2.0;
    let usable = (bounds.width - HANDLE).max(1.0);
    pad + ((v - MIN) / (MAX - MIN)).clamp(0.0, 1.0) * usable
}

impl canvas::Program<Message> for SpeedSlider {
    type State = bool;

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
                    Action::publish(Message::PlayRateChanged(Self::value_at(bounds, abs)))
                        .and_capture(),
                )
            }
            canvas::Event::Mouse(mouse::Event::CursorMoved { .. }) if *dragging => {
                let pos = cursor.position()?;
                Some(
                    Action::publish(Message::PlayRateChanged(Self::value_at(bounds, pos)))
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

    fn draw(
        &self,
        dragging: &bool,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> Vec<Geometry> {
        let show_all_labels = *dragging || cursor.is_over(bounds);
        let mut frame = Frame::new(renderer, bounds.size());
        let w = bounds.width;
        let h = bounds.height;
        if w <= 0.0 || h <= 0.0 {
            return vec![frame.into_geometry()];
        }
        let local = Rectangle {
            x: 0.0,
            y: 0.0,
            width: w,
            height: h,
        };
        let mid_y = h * 0.42;

        frame.stroke(
            &Path::line(
                Point::new(HANDLE / 2.0, mid_y),
                Point::new(w - HANDLE / 2.0, mid_y),
            ),
            Stroke::default()
                .with_color(Color {
                    a: 0.35,
                    ..self.p.muted
                })
                .with_width(3.0),
        );

        let one_x = to_x(local, 1.0);
        let val_x = to_x(local, self.value.clamp(MIN, MAX));
        frame.stroke(
            &Path::line(Point::new(one_x, mid_y), Point::new(val_x, mid_y)),
            Stroke::default().with_color(self.p.accent).with_width(3.0),
        );

        let fill_lo = one_x.min(val_x);
        let fill_hi = one_x.max(val_x);
        for s in SNAPS {
            let x = to_x(local, s);
            let near = (s - self.value).abs() < 0.04;
            let covered = near || (x >= fill_lo - 0.5 && x <= fill_hi + 0.5);
            let th = if near { 9.0 } else { 5.0 };
            frame.stroke(
                &Path::line(Point::new(x, mid_y - th), Point::new(x, mid_y + th)),
                Stroke::default()
                    .with_color(if covered {
                        self.p.accent
                    } else {
                        Color {
                            a: 0.5,
                            ..self.p.muted
                        }
                    })
                    .with_width(1.0),
            );
            if near || show_all_labels {
                let label = if (s.fract()).abs() < f32::EPSILON {
                    format!("{s:.0}\u{00d7}")
                } else {
                    format!("{s:.2}\u{00d7}")
                };
                frame.fill_text(Text {
                    content: label,
                    position: Point::new(x, mid_y + 12.0),
                    color: self.p.muted,
                    size: Pixels(8.0),
                    align_x: iced::alignment::Horizontal::Center.into(),
                    ..Text::default()
                });
            }
        }

        let center = Point::new(val_x, mid_y);
        for i in 0u16..4 {
            let t = f32::from(i) / 3.0;
            frame.fill(
                &Path::circle(center, HANDLE / 2.0 + 2.0 - t * 2.0),
                Color {
                    a: 0.08 + t * 0.16,
                    ..self.p.accent_glow
                },
            );
        }
        frame.fill(&Path::circle(center, HANDLE / 2.0), self.p.accent_strong);
        frame.stroke(
            &Path::circle(center, HANDLE / 2.0),
            Stroke::default()
                .with_color(Color {
                    a: 0.8,
                    ..self.p.bg
                })
                .with_width(2.0),
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
}

/// Build the interactive speed scrubber for the compact player.
pub(crate) fn speed_slider<'a>(value: f32, p: GuiPalette) -> Element<'a, Message> {
    Canvas::new(SpeedSlider { value, p })
        .width(Length::Fill)
        .height(Length::Fixed(34.0))
        .into()
}
