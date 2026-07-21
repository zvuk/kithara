use iced::{
    Color, Element, Event, Length, Point, Rectangle, Renderer, Size, Theme,
    alignment::Horizontal,
    mouse::{self, Cursor},
    widget::{
        Column, Row, Space,
        canvas::{self, Action, Canvas, Frame, Geometry, Path, Stroke},
        container,
    },
};
use num_traits::cast::AsPrimitive;

use crate::{
    render::{ReadValue, Skin, UiEvent, fonts, shaped_text},
    skin::FrameSkin,
    widgets::{
        Widget,
        behavior::{HoverState, ScalarDrag, ScalarDragMode, ScalarDragState},
    },
};

#[derive(bon::Builder)]
pub(crate) struct Crossfader<'path, 'value, 'data, 'skin> {
    path: &'path str,
    value: Option<&'value ReadValue<'data>>,
    skin: &'skin Skin,
}

impl<'a, 'path, 'value, 'data, 'skin> Widget<'a> for Crossfader<'path, 'value, 'data, 'skin>
where
    'skin: 'a,
{
    fn view(self) -> Element<'a, UiEvent> {
        let Some(ReadValue::Scalar(value)) = self.value else {
            return Space::new().into();
        };
        let metrics = &self.skin.crossfader;
        let label = |content: &'a str, alignment| {
            container(
                shaped_text(content)
                    .font(fonts::mono(metrics.label_text.weight))
                    .size(metrics.label_text.size)
                    .color(self.skin.color(metrics.label_color)),
            )
            .width(Length::Fill)
            .align_x(alignment)
            .into()
        };
        let labels = Row::with_children([
            label(&metrics.left_label, Horizontal::Left),
            label(&metrics.center_label, Horizontal::Center),
            label(&metrics.right_label, Horizontal::Right),
        ])
        .width(Length::Fill);
        let slider = Canvas::new(CrossfaderCanvas {
            drag: ScalarDrag::builder()
                .path(self.path.to_owned())
                .mode(ScalarDragMode::Horizontal)
                .hover(HoverState::new(mouse::Interaction::ResizingHorizontally))
                .build(),
            rail_background: self.skin.color(metrics.rail_background),
            rail_color: self.skin.color(metrics.rail_frame.border),
            rail_frame: metrics.rail_frame,
            rail_height: metrics.rail_height,
            thumb_color: self.skin.color(metrics.thumb_color),
            thumb_height: metrics.thumb_height,
            thumb_width: metrics.thumb_width,
            value: value.clamp(0.0, 1.0).as_(),
        })
        .width(Length::Fill)
        .height(Length::Fixed(metrics.thumb_height));

        Column::new()
            .push(labels)
            .push(slider)
            .spacing(metrics.label_gap)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }
}

struct CrossfaderCanvas {
    drag: ScalarDrag,
    rail_background: Color,
    rail_color: Color,
    rail_frame: FrameSkin,
    rail_height: f32,
    thumb_color: Color,
    thumb_height: f32,
    thumb_width: f32,
    value: f32,
}

impl canvas::Program<UiEvent> for CrossfaderCanvas {
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
        let rail_height = self.rail_height.min(bounds.height).max(0.0);
        let rail_point = Point::new(0.0, (bounds.height - rail_height) / 2.0);
        let rail_size = Size::new(bounds.width, rail_height);
        let rail = Path::rounded_rectangle(rail_point, rail_size, self.rail_frame.radius.into());
        frame.fill(&rail, self.rail_background);
        frame.stroke(
            &rail,
            Stroke::default()
                .with_color(self.rail_color)
                .with_width(self.rail_frame.border_width),
        );

        let thumb_width = self.thumb_width.min(bounds.width).max(0.0);
        let thumb_height = self.thumb_height.min(bounds.height).max(0.0);
        let travel = (bounds.width - thumb_width).max(0.0);
        frame.fill_rectangle(
            Point::new(
                (self.value * travel).round(),
                (bounds.height - thumb_height) / 2.0,
            ),
            Size::new(thumb_width, thumb_height),
            self.thumb_color,
        );
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
