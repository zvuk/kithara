use std::f32::consts::PI;

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
    registry::{ControlKindDesc, ValueKind},
    render::{ControlAction, ReadValue, RenderPalette, UiEvent},
    size::{Dim, SizeSpec},
};

struct Consts;

impl Consts {
    const BODY_RATIO: f32 = 0.68;
    const DRAG_RANGE: f32 = 140.0;
    const INDICATOR_WIDTH: f32 = 2.0;
    const OUTER_INSET: f32 = 3.0;
    const SIZE: f32 = 34.0;
    const START_ANGLE: f32 = 3.0 * PI / 4.0;
    const NEUTRAL_ANGLE: f32 = 3.0 * PI / 2.0;
    const SWEEP_ANGLE: f32 = 3.0 * PI / 2.0;
    const TRACK_WIDTH: f32 = 2.0;
    const BODY_STROKE_WIDTH: f32 = 1.0;
}

pub(crate) fn desc() -> ControlKindDesc {
    ControlKindDesc::new(Some(ValueKind::Scalar), Some(ValueKind::Scalar)).with_size(SizeSpec::new(
        Dim::Fixed(Consts::SIZE),
        Dim::Fixed(Consts::SIZE),
    ))
}

pub(crate) fn view<'a>(
    path: &str,
    value: Option<&ReadValue<'_>>,
    palette: RenderPalette,
) -> Element<'a, UiEvent> {
    let Some(ReadValue::Scalar(value)) = value else {
        return Space::new().into();
    };

    Canvas::new(Knob {
        palette,
        path: path.to_owned(),
        value: value.clamp(0.0, 1.0).as_(),
    })
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

struct Knob {
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
                .map(|position| scalar_action(&self.path, state, position.y)),
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
        let radius = (side / 2.0 - Consts::OUTER_INSET).max(0.0);
        let center = Point::new(bounds.width / 2.0, bounds.height / 2.0);
        let angle = Consts::START_ANGLE + Consts::SWEEP_ANGLE * self.value;

        if radius > 0.0 {
            draw_arc(
                &mut frame,
                center,
                radius,
                Consts::START_ANGLE,
                Consts::START_ANGLE + Consts::SWEEP_ANGLE,
                Color {
                    a: 0.9,
                    ..self.palette.line
                },
            );
            draw_arc(
                &mut frame,
                center,
                radius,
                Consts::NEUTRAL_ANGLE,
                angle,
                self.palette.accent,
            );

            let body_radius = Consts::BODY_RATIO * radius;
            let body = Path::circle(center, body_radius);
            frame.fill(&body, self.palette.bg_select);
            frame.stroke(
                &body,
                Stroke::default()
                    .with_color(self.palette.line)
                    .with_width(Consts::BODY_STROKE_WIDTH),
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
                    .with_width(Consts::INDICATOR_WIDTH),
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

fn scalar_action(path: &str, state: &DragState, current_y: f32) -> Action<UiEvent> {
    let value =
        (state.start_value + (state.start_y - current_y) / Consts::DRAG_RANGE).clamp(0.0, 1.0);
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
) {
    let path = Path::new(|builder| {
        builder.arc(Arc {
            center,
            radius,
            start_angle: Radians(start_angle),
            end_angle: Radians(end_angle),
        });
    });
    frame.stroke(
        &path,
        Stroke::default()
            .with_color(color)
            .with_width(Consts::TRACK_WIDTH),
    );
}
