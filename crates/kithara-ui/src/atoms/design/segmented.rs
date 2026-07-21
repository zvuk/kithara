use iced::{
    Color, Element, Event, Length, Point, Rectangle, Renderer, Size, Theme,
    alignment::{Horizontal, Vertical},
    mouse::{self, Button, Cursor},
    widget::{
        Space,
        canvas::{self, Action, Canvas, Frame, Geometry, Path, Stroke},
    },
};
use num_traits::ToPrimitive;

use crate::{
    render::{ControlAction, ReadValue, Skin, UiEvent, fonts},
    skin::SegmentedSkin,
    widgets::Widget,
};

#[derive(bon::Builder)]
pub(crate) struct Segmented<'a, 'value, 'data, 'skin> {
    path: &'a str,
    items: Vec<&'a str>,
    value: Option<&'value ReadValue<'data>>,
    skin: &'skin Skin,
}

impl<'a> Widget<'a> for Segmented<'a, '_, '_, '_> {
    fn view(self) -> Element<'a, UiEvent> {
        let Some(ReadValue::Scalar(value)) = self.value else {
            return Space::new().into();
        };
        let active = value
            .round()
            .to_usize()
            .filter(|index| *index < self.items.len());
        Canvas::new(SegmentedCanvas {
            path: self.path.to_owned(),
            items: self.items,
            active,
            metrics: self.skin.segmented,
            colors: SegmentedColors {
                background: self.skin.color(self.skin.segmented.background),
                active_background: self.skin.color(self.skin.segmented.active_background),
                active_text: self.skin.color(self.skin.segmented.active_text),
                inactive_text: self.skin.color(self.skin.segmented.inactive_text),
                frame: self.skin.color(self.skin.segmented.frame.border),
            },
        })
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
    }
}

struct SegmentedCanvas<'a> {
    path: String,
    items: Vec<&'a str>,
    active: Option<usize>,
    metrics: SegmentedSkin,
    colors: SegmentedColors,
}

#[derive(Clone, Copy)]
struct SegmentedColors {
    background: Color,
    active_background: Color,
    active_text: Color,
    inactive_text: Color,
    frame: Color,
}

impl canvas::Program<UiEvent> for SegmentedCanvas<'_> {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let Some(cell_width) = cell_width(bounds, self.items.len()) else {
            return vec![frame.into_geometry()];
        };
        frame.fill_rectangle(Point::ORIGIN, bounds.size(), self.colors.background);
        if let Some(active) = self.active {
            let x = index_position(active, cell_width);
            frame.fill_rectangle(
                Point::new(x, 0.0),
                Size::new(cell_width, bounds.height),
                self.colors.active_background,
            );
        }
        draw_grid(
            &mut frame,
            bounds,
            cell_width,
            self.items.len(),
            self.metrics,
            self.colors.frame,
        );
        for (index, item) in self.items.iter().enumerate() {
            let color = if self.active == Some(index) {
                self.colors.active_text
            } else {
                self.colors.inactive_text
            };
            frame.fill_text(canvas::Text {
                content: (*item).to_owned(),
                position: Point::new(
                    index_position(index, cell_width) + cell_width / 2.0,
                    bounds.height / 2.0,
                ),
                max_width: (cell_width - self.metrics.padding_x * 2.0).max(0.0),
                color,
                size: self.metrics.text.size.into(),
                font: fonts::mono(self.metrics.text.weight),
                align_x: Horizontal::Center.into(),
                align_y: Vertical::Center,
                shaping: iced::widget::text::Shaping::Advanced,
                ..canvas::Text::default()
            });
        }
        vec![frame.into_geometry()]
    }

    fn update(
        &self,
        _state: &mut (),
        event: &Event,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> Option<Action<UiEvent>> {
        let Event::Mouse(mouse::Event::ButtonPressed(Button::Left)) = event else {
            return None;
        };
        let position = cursor.position_in(bounds)?;
        let width = cell_width(bounds, self.items.len())?;
        let index = (position.x / width)
            .floor()
            .to_usize()?
            .min(self.items.len().saturating_sub(1));
        Some(
            Action::publish(UiEvent::Control {
                path: self.path.clone(),
                action: ControlAction::SelectIndex(index),
            })
            .and_capture(),
        )
    }

    fn mouse_interaction(
        &self,
        _state: &(),
        bounds: Rectangle,
        cursor: Cursor,
    ) -> mouse::Interaction {
        if self.items.is_empty() || !cursor.is_over(bounds) {
            mouse::Interaction::default()
        } else {
            mouse::Interaction::Pointer
        }
    }
}

fn cell_width(bounds: Rectangle, count: usize) -> Option<f32> {
    let count = count.to_f32()?;
    (count > 0.0 && bounds.width > 0.0).then_some(bounds.width / count)
}

fn index_position(index: usize, cell_width: f32) -> f32 {
    index.to_f32().map_or(0.0, |index| index * cell_width)
}

fn draw_grid(
    frame: &mut Frame,
    bounds: Rectangle,
    cell_width: f32,
    count: usize,
    metrics: SegmentedSkin,
    color: Color,
) {
    let width = metrics.frame.border_width;
    if width <= 0.0 {
        return;
    }
    let inset = width / 2.0;
    let frame_path = Path::rounded_rectangle(
        Point::new(inset, inset),
        Size::new(
            (bounds.width - width).max(0.0),
            (bounds.height - width).max(0.0),
        ),
        metrics.frame.radius.into(),
    );
    frame.stroke(
        &frame_path,
        Stroke::default().with_color(color).with_width(width),
    );
    for index in 1..count {
        let x = index_position(index, cell_width);
        frame.stroke(
            &Path::line(Point::new(x, 0.0), Point::new(x, bounds.height)),
            Stroke::default().with_color(color).with_width(width),
        );
    }
}
