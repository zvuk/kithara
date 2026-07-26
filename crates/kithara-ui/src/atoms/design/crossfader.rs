use iced::{
    Color, Element, Event, Length, Padding, Point, Rectangle, Renderer, Size, Theme,
    alignment::{Horizontal, Vertical},
    mouse::{self, Cursor},
    widget::{
        Column, Row, Space,
        canvas::{self, Action, Canvas, Frame, Geometry, Path, Stroke},
        container,
    },
};
use num_traits::cast::AsPrimitive;

use crate::{
    render::{Icon, ReadValue, Skin, UiEvent, fonts, shaped_text},
    skin::FrameSkin,
    widgets::{
        Widget,
        behavior::{HoverState, ScalarDrag, ScalarDragMode, ScalarDragState},
    },
};

#[derive(bon::Builder)]
pub(crate) struct Crossfader<'path, 'value, 'data, 'skin> {
    path: &'path str,
    ticks: bool,
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
        let letter = |content: &'a str| {
            shaped_text(content)
                .font(fonts::mono(metrics.letter_text.weight))
                .size(metrics.letter_text.size)
                .color(self.skin.color(metrics.letter_color))
        };
        let arrow =
            |icon: Icon| icon.view(metrics.arrow_size, self.skin.color(metrics.arrow_color));
        let side = |children: [Element<'a, UiEvent>; 2], alignment| {
            container(
                Row::with_children(children)
                    .spacing(metrics.arrow_gap)
                    .align_y(Vertical::Center),
            )
            .width(Length::Fill)
            .align_x(alignment)
            .into()
        };
        let labels = Row::with_children([
            side(
                [
                    letter(&metrics.left_label).into(),
                    arrow(Icon::ChevronsLeft),
                ],
                Horizontal::Left,
            ),
            container(
                shaped_text(&metrics.center_label)
                    .font(fonts::mono(metrics.label_text.weight))
                    .size(metrics.label_text.size)
                    .color(self.skin.color(metrics.label_color)),
            )
            .width(Length::Fill)
            .align_x(Horizontal::Center)
            .into(),
            side(
                [
                    arrow(Icon::ChevronsRight),
                    letter(&metrics.right_label).into(),
                ],
                Horizontal::Right,
            ),
        ])
        .width(Length::Fill);
        let ticks = TickRail {
            color: self.skin.color(metrics.tick_color),
            center_color: self.skin.color(metrics.tick_center_color),
            count: if self.ticks { metrics.tick_count } else { 0 },
            width: metrics.tick_width,
            height: metrics.tick_height,
            center_height: metrics.tick_center_height,
            gap: metrics.tick_gap,
            inset_x: metrics.tick_inset_x,
        };
        let slider_height = ticks.reserved() + metrics.thumb_height;
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
            thumb_notch_color: self.skin.color(metrics.thumb_notch_color),
            thumb_notch_height: metrics.thumb_notch_height,
            thumb_notch_width: metrics.thumb_notch_width,
            ticks,
            value: value.clamp(0.0, 1.0).as_(),
        })
        .width(Length::Fill)
        .height(Length::Fixed(slider_height));

        container(
            Column::new()
                .push(slider)
                .push(labels)
                .spacing(metrics.label_gap)
                .width(Length::Fill)
                .height(Length::Fill),
        )
        .padding(
            Padding::ZERO
                .top(metrics.padding_top)
                .bottom(metrics.padding_bottom)
                .left(metrics.padding_x)
                .right(metrics.padding_x),
        )
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
    thumb_notch_color: Color,
    thumb_notch_height: f32,
    thumb_notch_width: f32,
    ticks: TickRail,
    value: f32,
}

/// Scale above the rail: hairlines with a taller, brighter one at centre.
struct TickRail {
    color: Color,
    center_color: Color,
    count: usize,
    width: f32,
    height: f32,
    center_height: f32,
    gap: f32,
    inset_x: f32,
}

impl TickRail {
    /// Vertical space the rail and thumb are pushed down by.
    fn reserved(&self) -> f32 {
        if self.count < 2 {
            return 0.0;
        }
        self.center_height.max(self.height) + self.gap
    }

    fn draw(&self, frame: &mut Frame, width: f32) {
        if self.count < 2 {
            return;
        }
        let baseline = self.center_height.max(self.height);
        let travel = (width - self.inset_x * 2.0 - self.width).max(0.0);
        let last = self.count - 1;
        let center = last / 2;
        for index in 0..self.count {
            let is_center = self.count % 2 == 1 && index == center;
            let height = if is_center {
                self.center_height
            } else {
                self.height
            };
            let index: f32 = index.as_();
            let last: f32 = last.as_();
            frame.fill_rectangle(
                Point::new(self.inset_x + index / last * travel, baseline - height),
                Size::new(self.width, height),
                if is_center {
                    self.center_color
                } else {
                    self.color
                },
            );
        }
    }
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
        self.ticks.draw(&mut frame, bounds.width);
        let reserved = self.ticks.reserved();
        let track_height = (bounds.height - reserved).max(0.0);
        let rail_height = self.rail_height.min(track_height).max(0.0);
        let rail_point = Point::new(0.0, reserved + (track_height - rail_height) / 2.0);
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
        let thumb_height = self.thumb_height.min(track_height).max(0.0);
        let travel = (bounds.width - thumb_width).max(0.0);
        let thumb_x = (self.value * travel).round();
        let thumb_y = reserved + (track_height - thumb_height) / 2.0;
        frame.fill_rectangle(
            Point::new(thumb_x, thumb_y),
            Size::new(thumb_width, thumb_height),
            self.thumb_color,
        );
        let notch_height = self.thumb_notch_height.min(thumb_height);
        if notch_height > 0.0 && self.thumb_notch_width > 0.0 {
            frame.fill_rectangle(
                Point::new(
                    thumb_x + (thumb_width - self.thumb_notch_width) / 2.0,
                    thumb_y + (thumb_height - notch_height) / 2.0,
                ),
                Size::new(self.thumb_notch_width, notch_height),
                self.thumb_notch_color,
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
