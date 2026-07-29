use iced::{
    Alignment, Color, Element, Event, Length, Point, Radians, Rectangle, Renderer, Theme,
    mouse::{self, Cursor},
    widget::{
        Column, Space,
        canvas::{self, Action, Canvas, Frame, Geometry, Path, Stroke, path::Arc},
        container,
    },
};
use num_traits::cast::AsPrimitive;

use crate::{
    render::{ReadValue, Skin, UiEvent, typography::styled_text},
    skin::KnobSkin,
    widgets::{
        Widget,
        behavior::{HoverState, ScalarDrag, ScalarDragMode, ScalarDragState, WheelStep},
    },
};

#[derive(bon::Builder)]
pub(crate) struct Knob<'path, 'value, 'data, 'skin> {
    path: &'path str,
    label: Option<&'data str>,
    value: Option<&'value ReadValue<'data>>,
    skin: &'skin Skin,
}

impl<'a> Widget<'a> for Knob<'_, '_, '_, '_> {
    fn view(self) -> Element<'a, UiEvent> {
        let Some(ReadValue::Scalar(value)) = self.value else {
            return Space::new().into();
        };
        let value = value.clamp(0.0, 1.0).as_();
        let dial = Canvas::new(KnobCanvas {
            body_fill: self.skin.color(self.skin.knob.body_fill),
            body_border: self.skin.color(self.skin.knob.body_border),
            track_color: self.skin.color(self.skin.knob.track_color),
            value_color: self.skin.color(self.skin.knob.value_color),
            indicator_color: self.skin.color(self.skin.knob.indicator_color),
            drag: ScalarDrag::builder()
                .path(self.path.to_owned())
                .mode(ScalarDragMode::RelativeVertical {
                    value,
                    range: self.skin.knob.drag_range,
                })
                .hover(HoverState::new(mouse::Interaction::ResizingVertically))
                .double_click_value(0.5)
                .wheel(WheelStep {
                    value,
                    step: self.skin.knob.wheel_step,
                })
                .build(),
            metrics: self.skin.knob,
            value,
        })
        .width(Length::Fill)
        .height(Length::Fill);
        let caption = container(self.label.map_or_else(
            || Space::new().into(),
            |label| styled_text(label.to_owned(), self.skin.knob.label_text, self.skin),
        ))
        .height(Length::Fixed(self.skin.knob.label_height))
        .center_x(Length::Fill);

        Column::new()
            .push(dial)
            .push(caption)
            .spacing(self.skin.knob.label_gap)
            .align_x(Alignment::Center)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }
}

struct KnobCanvas {
    body_fill: Color,
    body_border: Color,
    track_color: Color,
    value_color: Color,
    indicator_color: Color,
    drag: ScalarDrag,
    metrics: KnobSkin,
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
                    ..self.track_color
                },
                self.metrics.track_width,
            );
            draw_arc(
                &mut frame,
                center,
                radius,
                self.metrics.neutral_angle,
                angle,
                self.value_color,
                self.metrics.track_width,
            );

            let body_radius = self.metrics.body_ratio * radius;
            let body = Path::circle(center, body_radius);
            frame.fill(&body, self.body_fill);
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
                    .with_color(self.indicator_color)
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

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{builtin, ids::SourceUri};

    #[kithara::test]
    fn the_knob_reserves_its_caption_row_with_and_without_a_label() {
        let origin = SourceUri("knob.kskin.ron".to_owned());
        let skin = Skin::resolve(builtin::skin_doc().clone(), &origin).unwrap();
        let value = ReadValue::Scalar(0.5);
        let rows = |label| {
            Knob::builder()
                .path("mixer/gain")
                .maybe_label(label)
                .value(&value)
                .skin(&skin)
                .build()
                .view()
                .as_widget()
                .children()
                .len()
        };

        assert_eq!(rows(Some("GAIN")), 2, "a captioned knob is dial plus label");
        assert_eq!(rows(None), rows(Some("GAIN")));
    }
}
