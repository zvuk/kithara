use iced::{
    Color, Element, Event, Length, Point, Radians, Rectangle, Renderer, Theme,
    mouse::{self, Cursor},
    widget::{
        Space,
        canvas::{self, Action, Canvas, Frame, Geometry, Path, Stroke, path::Arc},
    },
};
use num_traits::cast::AsPrimitive;

use crate::{
    render::{ReadValue, Skin, UiEvent, theme::RenderPalette},
    skin::KnobSkin,
    widgets::{
        Widget,
        behavior::{HoverState, ScalarDrag, ScalarDragMode, ScalarDragState},
    },
};

#[derive(bon::Builder)]
pub(crate) struct Knob<'path, 'value, 'data, 'skin> {
    path: &'path str,
    value: Option<&'value ReadValue<'data>>,
    skin: &'skin Skin,
}

impl<'a> Widget<'a> for Knob<'_, '_, '_, '_> {
    fn view(self) -> Element<'a, UiEvent> {
        let Some(ReadValue::Scalar(value)) = self.value else {
            return Space::new().into();
        };
        let value = value.clamp(0.0, 1.0).as_();
        Canvas::new(KnobCanvas {
            body_border: self.skin.color(self.skin.knob.body_border),
            drag: ScalarDrag::builder()
                .path(self.path.to_owned())
                .mode(ScalarDragMode::RelativeVertical {
                    value,
                    range: self.skin.knob.drag_range,
                })
                .hover(HoverState::new(mouse::Interaction::ResizingVertically))
                .build(),
            metrics: self.skin.knob,
            palette: self.skin.palette,
            value,
        })
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
    }
}

struct KnobCanvas {
    body_border: Color,
    drag: ScalarDrag,
    metrics: KnobSkin,
    palette: RenderPalette,
    value: f32,
}

impl canvas::Program<UiEvent> for KnobCanvas {
    type State = ScalarDragState;

    fn draw(
        &self,
        _state: &ScalarDragState,
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

    delegate::delegate! {
        to self.drag {
            fn update(
                &self,
                state: &mut ScalarDragState,
                event: &Event,
                bounds: Rectangle,
                cursor: Cursor,
            ) -> Option<Action<UiEvent>>;
            fn mouse_interaction(
                &self,
                state: &ScalarDragState,
                bounds: Rectangle,
                cursor: Cursor,
            ) -> mouse::Interaction;
        }
    }
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
