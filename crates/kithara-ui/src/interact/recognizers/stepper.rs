use kithara_platform::time::Instant;

use super::{
    super::{Hit, Input, Outcome, PointerInput, PointerPhase, Scroll},
    DoubleClick, wheel,
};

/// What a turn of a stepping surface amounts to. `By` is a delta, not a value:
/// the surface never knows what it is stepping.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum StepEvent {
    By(f32),
    Activate,
}

/// A surface that steps a value by detents, by trackpad travel and by dragging.
/// It owns no configuration, so the gesture and its state are one value.
#[derive(Default)]
pub(crate) struct Stepper {
    last_step: Option<Instant>,
    double_click: DoubleClick,
    drag: Option<f32>,
}

impl Stepper {
    /// How long one trackpad gesture owns the surface. A flick arrives as a
    /// long tail of shrinking pixel deltas, and every one of them would be its
    /// own detent without this.
    const STEP_INTERVAL_MS: u128 = 200;
    const DRAG_STEPS_PER_PIXEL: f32 = 0.25;

    pub(crate) fn on_input(
        &mut self,
        input: Input<'_>,
        hit: &Hit,
        now: Instant,
    ) -> Outcome<StepEvent> {
        match input {
            Input::Wheel(scroll) if hit.over() => {
                let steps = self.step(scroll, now);
                if steps == 0.0 {
                    return Outcome::captured();
                }
                Outcome::set(StepEvent::By(steps))
            }
            Input::Pointer(pointer) if pointer.phase == PointerPhase::Down => {
                let Some(position) = hit.inside() else {
                    return Outcome::IGNORED;
                };
                if self.double_click.register(position, now) {
                    self.drag = None;
                    return Outcome::set(StepEvent::Activate);
                }
                self.drag = Some(position.y);
                Outcome::IGNORED
            }
            Input::Pointer(PointerInput {
                phase: PointerPhase::Move,
                at: Some(at),
                ..
            }) => {
                let Some(steps) = self.drag_steps(at.y) else {
                    return Outcome::IGNORED;
                };
                if steps == 0.0 {
                    return Outcome::captured();
                }
                Outcome::set(StepEvent::By(steps))
            }
            Input::Pointer(pointer) if pointer.phase == PointerPhase::Up => {
                if self.drag.take().is_some() {
                    Outcome::captured()
                } else {
                    Outcome::IGNORED
                }
            }
            Input::InputMethod(_)
            | Input::KeyPressed { .. }
            | Input::KeyReleased { .. }
            | Input::ModifiersChanged(_)
            | Input::Pointer(_)
            | Input::Wheel(_) => Outcome::IGNORED,
        }
    }

    pub(crate) const fn dragging(&self) -> bool {
        self.drag.is_some()
    }

    fn drag_steps(&mut self, y: f32) -> Option<f32> {
        let from = self.drag?;
        self.drag = Some(y);
        Some((from - y) * Self::DRAG_STEPS_PER_PIXEL)
    }

    /// A line delta is one detent. A pixel delta is also one detent, not an
    /// accumulated fraction - this surface asks "has one gone by yet", which is
    /// a different question from [`wheel::steps`], which asks "how many have
    /// accumulated". The two policies do not merge.
    fn step(&mut self, scroll: Scroll, now: Instant) -> f32 {
        match scroll {
            Scroll::Lines { .. } => direction(scroll),
            Scroll::Pixels { y, .. } => {
                if self.last_step.is_some_and(|previous| {
                    now.saturating_duration_since(previous).as_millis() < Self::STEP_INTERVAL_MS
                }) {
                    return 0.0;
                }
                let step = direction(Scroll::lines(y));
                if step != 0.0 {
                    self.last_step = Some(now);
                }
                step
            }
        }
    }
}

fn direction(scroll: Scroll) -> f32 {
    let mut accum = 0.0;
    wheel::steps(&mut accum, scroll)
}

#[cfg(test)]
mod tests {
    use kithara_platform::time::Duration;
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        draw::{Pt, Rect},
        interact::mouse as mouse_input,
    };

    fn surface() -> Rect {
        Rect {
            h: 38.0,
            w: 80.0,
            x: 0.0,
            y: 0.0,
        }
    }

    fn at(y: f32) -> Hit {
        Hit::new(Some(Pt { x: 40.0, y }), surface())
    }

    fn inside() -> Hit {
        at(19.0)
    }

    fn moved(y: f32) -> Input<'static> {
        Input::Pointer(mouse_input(PointerPhase::Move, Some(Pt { x: 40.0, y })))
    }

    fn pointer(phase: PointerPhase) -> Input<'static> {
        Input::Pointer(mouse_input(phase, None))
    }

    #[kithara::test]
    fn a_detent_over_the_surface_reports_its_direction() {
        let now = Instant::now();

        for (delta, expected) in [(-1.0, 1.0), (1.0, -1.0)] {
            assert_eq!(
                Stepper::default().on_input(Input::Wheel(Scroll::lines(delta)), &inside(), now),
                Outcome::set(StepEvent::By(expected))
            );
        }
        assert_eq!(
            Stepper::default().on_input(Input::Wheel(Scroll::lines(-1.0)), &at(400.0), now),
            Outcome::IGNORED,
            "a detent outside the surface belongs to whatever is under it"
        );
    }

    #[kithara::test]
    fn a_trackpad_gesture_steps_once_and_its_momentum_adds_nothing() {
        let mut stepper = Stepper::default();
        let now = Instant::now();

        assert_eq!(
            stepper.on_input(Input::Wheel(Scroll::pixels(-14.0)), &inside(), now),
            Outcome::set(StepEvent::By(1.0)),
            "any pixel delta is one detent, not an accumulated fraction"
        );

        for tail in [-11.0, -8.0, -5.0, -3.0, -1.5, -0.6, -0.2] {
            assert_eq!(
                stepper.on_input(
                    Input::Wheel(Scroll::pixels(tail)),
                    &inside(),
                    now + Duration::from_millis(199),
                ),
                Outcome::captured(),
                "the momentum tail is owned but must not step the value"
            );
        }
        assert_eq!(
            stepper.on_input(
                Input::Wheel(Scroll::pixels(-11.0)),
                &inside(),
                now + Duration::from_millis(200),
            ),
            Outcome::set(StepEvent::By(1.0)),
            "the window is exclusive, so a delta 200 ms on is a new gesture"
        );
    }

    #[kithara::test]
    fn a_double_click_over_the_surface_activates_it() {
        let mut stepper = Stepper::default();
        let now = Instant::now();

        assert_eq!(
            stepper.on_input(pointer(PointerPhase::Down), &inside(), now),
            Outcome::IGNORED,
            "a lone press arms the drag without taking the pointer"
        );
        assert_eq!(
            stepper.on_input(pointer(PointerPhase::Down), &inside(), now),
            Outcome::set(StepEvent::Activate)
        );
        assert_eq!(
            stepper.on_input(moved(11.0), &at(11.0), now),
            Outcome::IGNORED,
            "the second press of a pair must not drag the value away from the reset"
        );
        assert_eq!(
            stepper.on_input(pointer(PointerPhase::Down), &inside(), now),
            Outcome::IGNORED,
            "the pair is spent, so the next press starts a new one"
        );
    }

    #[kithara::test]
    fn a_held_press_steps_the_value_by_the_travel_it_drags() {
        let mut stepper = Stepper::default();
        let now = Instant::now();

        assert_eq!(
            stepper.on_input(pointer(PointerPhase::Down), &inside(), now),
            Outcome::IGNORED
        );
        assert_eq!(
            stepper.on_input(moved(11.0), &at(11.0), now),
            Outcome::set(StepEvent::By(8.0 * Stepper::DRAG_STEPS_PER_PIXEL)),
            "dragging up must step the value up"
        );
        assert_eq!(
            stepper.on_input(moved(27.0), &at(27.0), now),
            Outcome::set(StepEvent::By(-16.0 * Stepper::DRAG_STEPS_PER_PIXEL)),
            "the travel is measured from where the last step left off"
        );

        assert_eq!(
            stepper.on_input(pointer(PointerPhase::Up), &at(27.0), now),
            Outcome::captured(),
            "release ends the drag"
        );
        assert_eq!(
            stepper.on_input(moved(3.0), &at(3.0), now),
            Outcome::IGNORED,
            "travel after the release belongs to nobody"
        );
    }

    #[kithara::test]
    fn the_surface_leaves_every_other_event_to_the_controls_it_wraps() {
        let now = Instant::now();

        for input in [pointer(PointerPhase::Up), moved(11.0)] {
            assert_eq!(
                Stepper::default().on_input(input, &inside(), now),
                Outcome::IGNORED
            );
        }
    }
}
