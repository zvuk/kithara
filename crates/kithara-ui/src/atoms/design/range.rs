use iced::{
    Color, Element, Event, Length, Point, Rectangle, Renderer, Size, Theme,
    mouse::{Button, Cursor, Event as MouseEvent, Interaction},
    widget::{
        Space,
        canvas::{self, Action, Canvas, Frame, Geometry},
    },
};

use crate::{
    render::{ControlAction, ScalarRange, Skin, UiEvent},
    skin::RangeSkin,
    widgets::Widget,
};

#[derive(bon::Builder)]
pub(crate) struct Range<'path, 'skin> {
    skin: &'skin Skin,
    path: &'path str,
    value: Option<ScalarRange>,
}

impl<'a> Widget<'a> for Range<'_, '_> {
    fn view(self) -> Element<'a, UiEvent> {
        let Some(value) = self.value else {
            return Space::new().into();
        };
        Canvas::new(RangeCanvas {
            metrics: self.skin.range,
            path: self.path.to_owned(),
            rail_background: self.skin.color(self.skin.range.rail_background),
            selection_color: self.skin.color(self.skin.range.selection_color),
            thumb_color: self.skin.color(self.skin.range.thumb_color),
            value: ScalarRange {
                min: value.min.clamp(0.0, 1.0),
                max: value.max.clamp(0.0, 1.0),
            },
        })
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
    }
}

#[derive(Clone, Copy)]
enum Handle {
    Min,
    Max,
}

#[derive(Default)]
pub(crate) struct RangeState {
    active: Option<Handle>,
}

struct RangeCanvas {
    metrics: RangeSkin,
    path: String,
    rail_background: Color,
    selection_color: Color,
    thumb_color: Color,
    value: ScalarRange,
}

impl canvas::Program<UiEvent> for RangeCanvas {
    type State = RangeState;

    fn draw(
        &self,
        _state: &RangeState,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let rail_y = (bounds.height - self.metrics.rail_height) / 2.0;
        frame.fill_rectangle(
            Point::new(0.0, rail_y),
            Size::new(bounds.width, self.metrics.rail_height),
            self.rail_background,
        );
        let min_x = self.value.min * bounds.width;
        let max_x = self.value.max * bounds.width;
        frame.fill_rectangle(
            Point::new(min_x, rail_y),
            Size::new((max_x - min_x).max(0.0), self.metrics.rail_height),
            self.selection_color,
        );
        let thumb_y = (bounds.height - self.metrics.thumb_height) / 2.0;
        for x in [min_x, max_x] {
            frame.fill_rectangle(
                Point::new(x - self.metrics.thumb_width / 2.0, thumb_y),
                Size::new(self.metrics.thumb_width, self.metrics.thumb_height),
                self.thumb_color,
            );
        }
        vec![frame.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        state: &RangeState,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> Interaction {
        if state.active.is_some() || cursor.is_over(bounds) {
            Interaction::ResizingHorizontally
        } else {
            Interaction::default()
        }
    }

    fn update(
        &self,
        state: &mut RangeState,
        event: &Event,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> Option<Action<UiEvent>> {
        match event {
            Event::Mouse(MouseEvent::ButtonPressed(Button::Left)) if cursor.is_over(bounds) => {
                let value = cursor_value(cursor, bounds)?;
                state.active = Some(nearest_handle(value, self.value));
                Some(self.publish(state.active?, value))
            }
            Event::Mouse(MouseEvent::CursorMoved { .. }) => {
                let handle = state.active?;
                Some(self.publish(handle, cursor_value(cursor, bounds)?))
            }
            Event::Mouse(MouseEvent::ButtonReleased(Button::Left)) if state.active.is_some() => {
                state.active = None;
                Some(Action::capture())
            }
            _ => None,
        }
    }
}

impl RangeCanvas {
    fn publish(&self, handle: Handle, value: f32) -> Action<UiEvent> {
        let suffix = match handle {
            Handle::Min => "min",
            Handle::Max => "max",
        };
        Action::publish(UiEvent::Control {
            path: format!("{}/{suffix}", self.path),
            action: ControlAction::SetScalar(f64::from(value)),
        })
        .and_capture()
    }
}

fn cursor_value(cursor: Cursor, bounds: Rectangle) -> Option<f32> {
    let position = cursor.position_in(bounds)?;
    (bounds.width > 0.0).then_some((position.x / bounds.width).clamp(0.0, 1.0))
}

fn nearest_handle(value: f32, range: ScalarRange) -> Handle {
    if (value - range.min).abs() <= (value - range.max).abs() {
        Handle::Min
    } else {
        Handle::Max
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_handle_breaks_a_tie_toward_minimum() {
        let range = ScalarRange { min: 0.2, max: 0.8 };

        assert!(matches!(nearest_handle(0.5, range), Handle::Min));
        assert!(matches!(nearest_handle(0.7, range), Handle::Max));
    }
}
