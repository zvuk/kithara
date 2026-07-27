use iced::{
    Element, Event, Length, Rectangle, Renderer, Theme,
    mouse::{self, Button, Cursor, ScrollDelta},
    time::Instant,
    widget::canvas::{self, Action, Canvas, Geometry},
};

use crate::{
    render::{ControlAction, UiEvent},
    widgets::{
        Widget,
        behavior::{DoubleClickState, HoverState},
    },
};

const WHEEL_STEP_INTERVAL_MS: u128 = 200;
const DRAG_STEPS_PER_PIXEL: f32 = 0.25;
const HOVER: HoverState = HoverState::new(mouse::Interaction::ResizingVertically);

#[derive(bon::Builder)]
pub(crate) struct WheelSurface<'path> {
    path: &'path str,
}

impl<'a> Widget<'a> for WheelSurface<'_> {
    fn view(self) -> Element<'a, UiEvent> {
        Canvas::new(WheelCanvas {
            path: self.path.to_owned(),
        })
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
    }
}

struct WheelCanvas {
    path: String,
}

#[derive(Default)]
struct WheelState {
    last_step: Option<Instant>,
    double_click: DoubleClickState,
    drag: Option<f32>,
}

impl WheelState {
    fn drag_steps(&mut self, y: f32) -> Option<f32> {
        let from = self.drag?;
        self.drag = Some(y);
        Some((from - y) * DRAG_STEPS_PER_PIXEL)
    }

    fn step(&mut self, delta: ScrollDelta) -> f32 {
        match delta {
            ScrollDelta::Lines { y, .. } => direction(y),
            ScrollDelta::Pixels { y, .. } => {
                let now = Instant::now();
                if self.last_step.is_some_and(|previous| {
                    now.checked_duration_since(previous)
                        .is_some_and(|elapsed| elapsed.as_millis() < WHEEL_STEP_INTERVAL_MS)
                }) {
                    return 0.0;
                }
                let step = direction(y);
                if step != 0.0 {
                    self.last_step = Some(now);
                }
                step
            }
        }
    }
}

fn direction(y: f32) -> f32 {
    if y == 0.0 { 0.0 } else { -y.signum() }
}

impl WheelCanvas {
    fn publish(&self, action: ControlAction) -> Action<UiEvent> {
        Action::publish(UiEvent::Control {
            path: self.path.clone(),
            action,
        })
        .and_capture()
    }
}

impl canvas::Program<UiEvent> for WheelCanvas {
    type State = WheelState;

    fn draw(
        &self,
        _state: &WheelState,
        _renderer: &Renderer,
        _theme: &Theme,
        _bounds: Rectangle,
        _cursor: Cursor,
    ) -> Vec<Geometry> {
        Vec::new()
    }

    fn mouse_interaction(
        &self,
        state: &WheelState,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> mouse::Interaction {
        HOVER.interaction(state.drag.is_some(), bounds, cursor)
    }

    fn update(
        &self,
        state: &mut WheelState,
        event: &Event,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> Option<Action<UiEvent>> {
        match event {
            Event::Mouse(mouse::Event::WheelScrolled { delta }) if cursor.is_over(bounds) => {
                let steps = state.step(*delta);
                if steps == 0.0 {
                    return Some(Action::capture());
                }
                Some(self.publish(ControlAction::StepScalar(steps)))
            }
            Event::Mouse(mouse::Event::ButtonPressed(Button::Left)) if cursor.is_over(bounds) => {
                let position = cursor.position()?;
                if state.double_click.register(position) {
                    state.drag = None;
                    return Some(self.publish(ControlAction::Activate));
                }
                state.drag = Some(position.y);
                None
            }
            Event::Mouse(mouse::Event::CursorMoved { position }) => {
                let steps = state.drag_steps(position.y)?;
                if steps == 0.0 {
                    return Some(Action::capture());
                }
                Some(self.publish(ControlAction::StepScalar(steps)))
            }
            Event::Mouse(mouse::Event::ButtonReleased(Button::Left)) => {
                state.drag.take().is_some().then(Action::capture)
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use iced::{Point, Size, widget::canvas::Program};
    use kithara_test_utils::kithara;

    use super::*;

    fn surface() -> WheelCanvas {
        WheelCanvas {
            path: "deck-a/tempo".to_owned(),
        }
    }

    fn bounds() -> Rectangle {
        Rectangle::new(Point::ORIGIN, Size::new(80.0, 38.0))
    }

    fn at(y: f32) -> Cursor {
        Cursor::Available(Point::new(40.0, y))
    }

    fn inside() -> Cursor {
        at(19.0)
    }

    fn wheel(y: f32) -> Event {
        Event::Mouse(mouse::Event::WheelScrolled {
            delta: ScrollDelta::Lines { x: 0.0, y },
        })
    }

    fn pixels(y: f32) -> Event {
        Event::Mouse(mouse::Event::WheelScrolled {
            delta: ScrollDelta::Pixels { x: 0.0, y },
        })
    }

    fn press() -> Event {
        Event::Mouse(mouse::Event::ButtonPressed(Button::Left))
    }

    fn release() -> Event {
        Event::Mouse(mouse::Event::ButtonReleased(Button::Left))
    }

    fn moved(y: f32) -> Event {
        Event::Mouse(mouse::Event::CursorMoved {
            position: Point::new(40.0, y),
        })
    }

    fn stepped(steps: f32) -> UiEvent {
        UiEvent::Control {
            path: "deck-a/tempo".to_owned(),
            action: ControlAction::StepScalar(steps),
        }
    }

    fn published(
        surface: &WheelCanvas,
        state: &mut WheelState,
        event: &Event,
        cursor: Cursor,
    ) -> Option<UiEvent> {
        surface
            .update(state, event, bounds(), cursor)
            .and_then(|action| action.into_inner().0)
    }

    #[kithara::test]
    fn a_detent_over_the_surface_reports_its_direction() {
        let surface = surface();
        let stepped = |event| published(&surface, &mut WheelState::default(), &event, inside());

        assert_eq!(
            stepped(wheel(-1.0)),
            Some(UiEvent::Control {
                path: "deck-a/tempo".to_owned(),
                action: ControlAction::StepScalar(1.0),
            })
        );
        assert_eq!(
            stepped(wheel(1.0)),
            Some(UiEvent::Control {
                path: "deck-a/tempo".to_owned(),
                action: ControlAction::StepScalar(-1.0),
            })
        );
        assert!(
            surface
                .update(
                    &mut WheelState::default(),
                    &wheel(-1.0),
                    bounds(),
                    Cursor::Available(Point::new(400.0, 19.0)),
                )
                .is_none(),
            "a detent outside the surface belongs to whatever is under it"
        );
    }

    #[kithara::test]
    fn a_trackpad_gesture_steps_once_and_its_momentum_adds_nothing() {
        let surface = surface();
        let mut state = WheelState::default();

        assert_eq!(
            published(&surface, &mut state, &pixels(-14.0), inside()),
            Some(UiEvent::Control {
                path: "deck-a/tempo".to_owned(),
                action: ControlAction::StepScalar(1.0),
            })
        );

        for tail in [-11.0, -8.0, -5.0, -3.0, -1.5, -0.6, -0.2] {
            let action = surface
                .update(&mut state, &pixels(tail), bounds(), inside())
                .unwrap_or_else(|| panic!("the surface owns every pixel delta over it"));
            assert_eq!(
                action.into_inner().0,
                None,
                "the momentum tail must not step the value"
            );
        }
    }

    #[kithara::test]
    fn a_double_click_over_the_surface_activates_it() {
        let surface = surface();
        let mut state = WheelState::default();

        assert_eq!(published(&surface, &mut state, &press(), inside()), None);
        assert_eq!(
            published(&surface, &mut state, &press(), inside()),
            Some(UiEvent::Control {
                path: "deck-a/tempo".to_owned(),
                action: ControlAction::Activate,
            })
        );
        assert!(
            surface
                .update(&mut state, &moved(11.0), bounds(), at(11.0))
                .is_none(),
            "the second press of a pair must not drag the value away from the reset"
        );
        assert_eq!(
            published(&surface, &mut state, &press(), inside()),
            None,
            "the pair is spent, so the next press starts a new one"
        );
    }

    #[kithara::test]
    fn a_held_press_steps_the_value_by_the_travel_it_drags() {
        let surface = surface();
        let mut state = WheelState::default();

        assert_eq!(published(&surface, &mut state, &press(), inside()), None);
        assert_eq!(
            published(&surface, &mut state, &moved(11.0), at(11.0)),
            Some(stepped(8.0 * DRAG_STEPS_PER_PIXEL)),
            "dragging up must step the value up"
        );
        assert_eq!(
            published(&surface, &mut state, &moved(27.0), at(27.0)),
            Some(stepped(-16.0 * DRAG_STEPS_PER_PIXEL)),
            "the travel is measured from where the last step left off"
        );

        let released = surface
            .update(&mut state, &release(), bounds(), at(27.0))
            .unwrap_or_else(|| panic!("release must end the drag"));
        assert_eq!(released.into_inner().0, None);
        assert!(
            surface
                .update(&mut state, &moved(3.0), bounds(), at(3.0))
                .is_none(),
            "travel after the release belongs to nobody"
        );
    }

    #[kithara::test]
    fn the_surface_leaves_every_other_event_to_the_controls_it_wraps() {
        for event in [release(), moved(11.0)] {
            assert!(
                surface()
                    .update(&mut WheelState::default(), &event, bounds(), inside())
                    .is_none()
            );
        }
    }
}
