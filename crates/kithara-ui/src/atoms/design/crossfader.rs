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
    skin::{FrameSkin, TickSkin},
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
        let ticks = self
            .ticks
            .then(|| TickRail::new(TickAxis::Horizontal, metrics.ticks, self.skin));
        let slider_height = ticks.as_ref().map_or(0.0, TickRail::reserved) + metrics.thumb_height;
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
    ticks: Option<TickRail>,
    value: f32,
}

#[derive(Clone, Copy)]
pub(crate) enum TickAxis {
    Horizontal,
    Vertical,
}

pub(crate) struct TickRail {
    axis: TickAxis,
    metrics: TickSkin,
    color: Color,
    center_color: Color,
}

impl TickRail {
    pub(crate) fn new(axis: TickAxis, metrics: TickSkin, skin: &Skin) -> Self {
        Self {
            axis,
            metrics,
            color: skin.color(metrics.color),
            center_color: skin.color(metrics.center_color),
        }
    }

    fn last(&self) -> Option<usize> {
        self.metrics.count.checked_sub(1).filter(|last| *last > 0)
    }

    pub(crate) fn extent(&self) -> f32 {
        self.metrics.center_length.max(self.metrics.length)
    }

    pub(crate) fn reserved(&self) -> f32 {
        self.last()
            .map_or(0.0, |_| self.extent() + self.metrics.gap)
    }

    pub(crate) fn draw(&self, frame: &mut Frame, rail: Rectangle) {
        let Some(last) = self.last() else {
            return;
        };
        let (span, cross) = match self.axis {
            TickAxis::Horizontal => (rail.width, rail.height),
            TickAxis::Vertical => (rail.height, rail.width),
        };
        let cross = cross.max(0.0);
        let travel = (span - self.metrics.inset * 2.0 - self.metrics.thickness).max(0.0);
        let center = last / 2;
        let steps: f32 = last.as_();
        for index in 0..self.metrics.count {
            let is_center = self.metrics.count % 2 == 1 && index == center;
            let length = if is_center {
                self.metrics.center_length
            } else {
                self.metrics.length
            }
            .min(cross);
            let step: f32 = index.as_();
            let offset = self.metrics.inset + step / steps * travel;
            let color = if is_center {
                self.center_color
            } else {
                self.color
            };
            let (corner, size) = match self.axis {
                TickAxis::Horizontal => (
                    Point::new(rail.x + offset, rail.y + cross - length),
                    Size::new(self.metrics.thickness, length),
                ),
                TickAxis::Vertical => (
                    Point::new(rail.x + cross - length, rail.y + offset),
                    Size::new(length, self.metrics.thickness),
                ),
            };
            frame.fill_rectangle(corner, size, color);
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
        let reserved = match &self.ticks {
            Some(ticks) => {
                ticks.draw(
                    &mut frame,
                    Rectangle {
                        x: 0.0,
                        y: 0.0,
                        width: bounds.width,
                        height: ticks.extent(),
                    },
                );
                ticks.reserved()
            }
            None => 0.0,
        };
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
