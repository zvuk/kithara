use iced::{
    Color, Element, Event, Length, Point, Radians, Rectangle, Renderer, Theme,
    mouse::{self, Button, Cursor},
    widget::{
        Space,
        canvas::{self, Action, Canvas, Frame, Geometry, Path, Stroke, path::Arc},
    },
};
use num_traits::cast::AsPrimitive;

use crate::{
    render::{ControlAction, ReadValue, Skin, UiEvent, theme::RenderPalette},
    skin::KnobSkin,
};

pub(crate) fn view<'a>(
    path: &str,
    value: Option<&ReadValue<'_>>,
    skin: &Skin,
) -> Element<'a, UiEvent> {
    let Some(ReadValue::Scalar(value)) = value else {
        return Space::new().into();
    };

    Canvas::new(Knob {
        body_border: skin.color(skin.knob.body_border),
        metrics: skin.knob,
        palette: skin.palette,
        path: path.to_owned(),
        value: value.clamp(0.0, 1.0).as_(),
    })
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

struct Knob {
    body_border: Color,
    metrics: KnobSkin,
    palette: RenderPalette,
    path: String,
    value: f32,
}

#[derive(Default)]
struct DragState {
    active: bool,
    start_value: f32,
    start_y: f32,
}

impl canvas::Program<UiEvent> for Knob {
    type State = DragState;

    fn update(
        &self,
        state: &mut DragState,
        event: &Event,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> Option<Action<UiEvent>> {
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(Button::Left)) if cursor.is_over(bounds) => {
                let position = cursor.position()?;
                state.active = true;
                state.start_value = self.value;
                state.start_y = position.y;
                Some(Action::capture())
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) if state.active => cursor
                .position()
                .map(|position| scalar_action(&self.path, state, position.y, self.metrics)),
            Event::Mouse(mouse::Event::ButtonReleased(Button::Left)) if state.active => {
                state.active = false;
                Some(Action::capture())
            }
            _ => None,
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
        let side = bounds.width.min(bounds.height);
        let radius = (side / 2.0 - self.metrics.outer_inset).max(0.0);
        let center = Point::new(bounds.width / 2.0, bounds.height / 2.0);
        let angle = self.metrics.start_angle + self.metrics.sweep_angle * self.value;

        if radius > 0.0 {
            draw_arc(
                &mut frame,
                center,
                radius,
                self.metrics.start_angle,
                self.metrics.start_angle + self.metrics.sweep_angle,
                Color {
                    a: self.metrics.track_alpha,
                    ..self.palette.line
                },
                self.metrics.track_width,
            );
            draw_arc(
                &mut frame,
                center,
                radius,
                self.metrics.neutral_angle,
                angle,
                self.palette.accent,
                self.metrics.track_width,
            );

            let body_radius = self.metrics.body_ratio * radius;
            let body = Path::circle(center, body_radius);
            frame.fill(&body, self.palette.bg_select);
            frame.stroke(
                &body,
                Stroke::default()
                    .with_color(self.body_border)
                    .with_width(self.metrics.body_border_width),
            );
            frame.stroke(
                &Path::line(
                    center,
                    Point::new(
                        center.x + angle.cos() * body_radius,
                        center.y + angle.sin() * body_radius,
                    ),
                ),
                Stroke::default()
                    .with_color(self.palette.text)
                    .with_width(self.metrics.indicator_width),
            );
        }

        vec![frame.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        state: &DragState,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> mouse::Interaction {
        if state.active || cursor.is_over(bounds) {
            mouse::Interaction::ResizingVertically
        } else {
            mouse::Interaction::default()
        }
    }
}

fn scalar_action(
    path: &str,
    state: &DragState,
    current_y: f32,
    metrics: KnobSkin,
) -> Action<UiEvent> {
    let value =
        (state.start_value + (state.start_y - current_y) / metrics.drag_range).clamp(0.0, 1.0);
    Action::publish(UiEvent::Control {
        path: path.to_owned(),
        action: ControlAction::SetScalar(f64::from(value)),
    })
    .and_capture()
}

fn draw_arc(
    frame: &mut Frame,
    center: Point,
    radius: f32,
    start_angle: f32,
    end_angle: f32,
    color: Color,
    width: f32,
) {
    let path = Path::new(|builder| {
        builder.arc(Arc {
            center,
            radius,
            start_angle: Radians(start_angle),
            end_angle: Radians(end_angle),
        });
    });
    frame.stroke(&path, Stroke::default().with_color(color).with_width(width));
}
