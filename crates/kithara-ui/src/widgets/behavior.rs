use iced::{
    Event, Point, Rectangle,
    mouse::{self, Button, Cursor},
    time::Instant,
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
    RelativeHorizontal { value: f32, scale: f32 },
    Vertical,
    RelativeVertical { value: f32, range: f32 },
}

#[derive(bon::Builder)]
pub(crate) struct ScalarDrag {
    path: String,
    mode: ScalarDragMode,
    hover: HoverState,
    double_click_value: Option<f32>,
}

#[derive(Default)]
struct DoubleClickState {
    previous: Option<(Point, Instant)>,
}

impl DoubleClickState {
    fn register(&mut self, position: Point) -> bool {
        let now = Instant::now();
        let consecutive = self
            .previous
            .is_some_and(|(previous_position, previous_time)| {
                previous_position.distance(position) < 6.0
                    && now
                        .checked_duration_since(previous_time)
                        .is_some_and(|duration| duration.as_millis() <= 300)
            });
        self.previous = (!consecutive).then_some((position, now));
        consecutive
    }
}

#[derive(Default)]
pub(crate) struct ScalarDragState {
    active: bool,
    start_position: f32,
    start_value: f32,
    double_click: DoubleClickState,
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
            ScalarDragMode::Vertical
            | ScalarDragMode::RelativeHorizontal { .. }
            | ScalarDragMode::RelativeVertical { .. } => return None,
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

    fn double_click_action(
        &self,
        state: &mut ScalarDragState,
        cursor: Cursor,
    ) -> Option<Action<UiEvent>> {
        let value = self.double_click_value?;
        state
            .double_click
            .register(cursor.position()?)
            .then(|| self.publish(value))
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
                if let Some(action) = self.double_click_action(state, cursor) {
                    return Some(action);
                }
                match self.mode {
                    ScalarDragMode::RelativeHorizontal { value, .. } => {
                        state.start_position = cursor.position()?.x;
                        state.start_value = value;
                        state.active = true;
                        Some(Action::capture())
                    }
                    ScalarDragMode::RelativeVertical { value, .. } => {
                        state.start_position = cursor.position()?.y;
                        state.start_value = value;
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
                ScalarDragMode::RelativeHorizontal { scale, .. } if bounds.width > 0.0 => {
                    cursor.position().map(|position| {
                        self.publish(
                            (state.start_value
                                - (position.x - state.start_position) / bounds.width * scale)
                                .clamp(0.0, 1.0),
                        )
                    })
                }
                ScalarDragMode::RelativeVertical { range, .. } => {
                    cursor.position().map(|position| {
                        self.publish(
                            (state.start_value + (state.start_position - position.y) / range)
                                .clamp(0.0, 1.0),
                        )
                    })
                }
                ScalarDragMode::Horizontal | ScalarDragMode::Vertical => {
                    self.absolute_action(bounds, cursor)
                }
                ScalarDragMode::HorizontalClick | ScalarDragMode::RelativeHorizontal { .. } => None,
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

    #[kithara::test]
    fn horizontal_drag_publishes_normalized_scalar() {
        let drag = ScalarDrag::builder()
            .path("volume".to_owned())
            .mode(ScalarDragMode::Horizontal)
            .hover(HoverState::new(mouse::Interaction::ResizingHorizontally))
            .build();
        let bounds = Rectangle::new(Point::ORIGIN, iced::Size::new(64.0, 22.0));
        let cursor = Cursor::Available(Point::new(16.0, 11.0));
        let press = Event::Mouse(mouse::Event::ButtonPressed(Button::Left));
        let mut state = ScalarDragState::default();

        let action = drag
            .update(&mut state, &press, bounds, cursor)
            .unwrap_or_else(|| panic!("horizontal drag must publish an action"));
        let (message, _, _) = action.into_inner();

        assert_eq!(
            message,
            Some(UiEvent::Control {
                path: "volume".to_owned(),
                action: ControlAction::SetScalar(0.25),
            })
        );
    }

    #[kithara::test]
    fn relative_drag_double_click_resets_to_configured_value() {
        let drag = ScalarDrag::builder()
            .path("knob".to_owned())
            .mode(ScalarDragMode::RelativeVertical {
                value: 0.8,
                range: 140.0,
            })
            .hover(HoverState::new(mouse::Interaction::ResizingVertically))
            .double_click_value(0.5)
            .build();
        let bounds = Rectangle::new(Point::ORIGIN, iced::Size::new(34.0, 34.0));
        let cursor = Cursor::Available(Point::new(17.0, 17.0));
        let press = Event::Mouse(mouse::Event::ButtonPressed(Button::Left));
        let release = Event::Mouse(mouse::Event::ButtonReleased(Button::Left));
        let mut state = ScalarDragState::default();

        assert!(drag.update(&mut state, &press, bounds, cursor).is_some());
        assert!(drag.update(&mut state, &release, bounds, cursor).is_some());
        let action = drag.update(&mut state, &press, bounds, cursor).unwrap();
        let (message, _, _) = action.into_inner();

        assert_eq!(
            message,
            Some(UiEvent::Control {
                path: "knob".to_owned(),
                action: ControlAction::SetScalar(0.5),
            })
        );
    }

    #[kithara::test]
    fn relative_horizontal_drag_uses_start_value_without_click_seek() {
        let drag = ScalarDrag::builder()
            .path("wave".to_owned())
            .mode(ScalarDragMode::RelativeHorizontal {
                value: 0.4,
                scale: 0.2,
            })
            .hover(HoverState::new(mouse::Interaction::Grab))
            .build();
        let bounds = Rectangle::new(Point::ORIGIN, iced::Size::new(200.0, 40.0));
        let press = Event::Mouse(mouse::Event::ButtonPressed(Button::Left));
        let release = Event::Mouse(mouse::Event::ButtonReleased(Button::Left));
        let mut state = ScalarDragState::default();

        let pressed = drag
            .update(
                &mut state,
                &press,
                bounds,
                Cursor::Available(Point::new(100.0, 20.0)),
            )
            .unwrap_or_else(|| panic!("relative drag must capture its press"));
        assert_eq!(pressed.into_inner().0, None);

        let moved = drag
            .update(
                &mut state,
                &Event::Mouse(mouse::Event::CursorMoved {
                    position: Point::new(150.0, 20.0),
                }),
                bounds,
                Cursor::Available(Point::new(150.0, 20.0)),
            )
            .unwrap_or_else(|| panic!("relative drag must publish movement"));
        let Some(UiEvent::Control {
            path,
            action: ControlAction::SetScalar(value),
        }) = moved.into_inner().0
        else {
            panic!("relative drag must publish a scalar");
        };
        assert_eq!(path, "wave");
        assert!((value - 0.35).abs() < 0.000_1);

        let released = drag
            .update(
                &mut state,
                &release,
                bounds,
                Cursor::Available(Point::new(150.0, 20.0)),
            )
            .unwrap_or_else(|| panic!("relative drag must capture its release"));
        assert_eq!(released.into_inner().0, None);
    }
}
