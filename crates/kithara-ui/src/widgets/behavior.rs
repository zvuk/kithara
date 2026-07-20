use iced::{
    Event, Point, Rectangle,
    mouse::{self, Button, Cursor},
    widget::canvas::Action,
};

use crate::render::{ControlAction, UiEvent};

#[derive(Clone, Copy)]
pub(crate) struct HoverState {
    interaction: mouse::Interaction,
}

impl HoverState {
    pub(crate) const fn new(interaction: mouse::Interaction) -> Self {
        Self { interaction }
    }

    pub(crate) fn interaction(
        self,
        active: bool,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> mouse::Interaction {
        if active || cursor.is_over(bounds) {
            self.interaction
        } else {
            mouse::Interaction::default()
        }
    }
}

#[derive(bon::Builder)]
pub(crate) struct ClickActivate {
    path: String,
    hover: HoverState,
}

impl ClickActivate {
    pub(crate) fn mouse_interaction(
        &self,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> mouse::Interaction {
        self.hover.interaction(false, bounds, cursor)
    }

    pub(crate) fn update(
        &self,
        _state: &mut (),
        event: &Event,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> Option<Action<UiEvent>> {
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(Button::Left)) if cursor.is_over(bounds) => {
                Some(
                    Action::publish(UiEvent::Control {
                        path: self.path.clone(),
                        action: ControlAction::Activate,
                    })
                    .and_capture(),
                )
            }
            _ => None,
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) enum ScalarDragMode {
    Horizontal,
    HorizontalClick,
    Vertical,
    RelativeVertical { value: f32, range: f32 },
}

#[derive(bon::Builder)]
pub(crate) struct ScalarDrag {
    path: String,
    mode: ScalarDragMode,
    hover: HoverState,
}

#[derive(Default)]
pub(crate) struct ScalarDragState {
    active: bool,
    start_position: f32,
}

impl ScalarDrag {
    fn absolute_action(&self, bounds: Rectangle, cursor: Cursor) -> Option<Action<UiEvent>> {
        let position = cursor.position()?;
        let value = match self.mode {
            ScalarDragMode::Horizontal | ScalarDragMode::HorizontalClick => {
                normalized_horizontal(bounds, position)?
            }
            ScalarDragMode::Vertical if bounds.height > 0.0 => {
                1.0 - (position.y - bounds.y) / bounds.height
            }
            ScalarDragMode::Vertical | ScalarDragMode::RelativeVertical { .. } => return None,
        };
        Some(self.publish(value.clamp(0.0, 1.0)))
    }

    pub(crate) fn mouse_interaction(
        &self,
        state: &ScalarDragState,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> mouse::Interaction {
        self.hover.interaction(state.active, bounds, cursor)
    }

    fn publish(&self, value: f32) -> Action<UiEvent> {
        Action::publish(UiEvent::Control {
            path: self.path.clone(),
            action: ControlAction::SetScalar(f64::from(value)),
        })
        .and_capture()
    }

    pub(crate) fn update(
        &self,
        state: &mut ScalarDragState,
        event: &Event,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> Option<Action<UiEvent>> {
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(Button::Left)) if cursor.is_over(bounds) => {
                match self.mode {
                    ScalarDragMode::RelativeVertical { .. } => {
                        state.start_position = cursor.position()?.y;
                        state.active = true;
                        Some(Action::capture())
                    }
                    ScalarDragMode::Horizontal | ScalarDragMode::Vertical => {
                        state.active = true;
                        self.absolute_action(bounds, cursor)
                    }
                    ScalarDragMode::HorizontalClick => self.absolute_action(bounds, cursor),
                }
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) if state.active => match self.mode {
                ScalarDragMode::RelativeVertical { value, range } => {
                    cursor.position().map(|position| {
                        self.publish(
                            (value + (state.start_position - position.y) / range).clamp(0.0, 1.0),
                        )
                    })
                }
                ScalarDragMode::Horizontal | ScalarDragMode::Vertical => {
                    self.absolute_action(bounds, cursor)
                }
                ScalarDragMode::HorizontalClick => None,
            },
            Event::Mouse(mouse::Event::ButtonReleased(Button::Left)) if state.active => {
                state.active = false;
                Some(Action::capture())
            }
            _ => None,
        }
    }
}

fn normalized_horizontal(bounds: Rectangle, position: Point) -> Option<f32> {
    (bounds.width > 0.0).then(|| ((position.x - bounds.x) / bounds.width).clamp(0.0, 1.0))
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn horizontal_click_maps_to_normalized_position() {
        let bounds = Rectangle::new(Point::new(20.0, 4.0), iced::Size::new(200.0, 40.0));

        for (x, expected) in [
            (0.0, 0.0),
            (20.0, 0.0),
            (120.0, 0.5),
            (220.0, 1.0),
            (260.0, 1.0),
        ] {
            assert_eq!(
                normalized_horizontal(bounds, Point::new(x, 10.0)),
                Some(expected)
            );
        }
    }
}
