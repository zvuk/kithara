use kithara_platform::time::Instant;

use super::{
    super::{Hit, Input, Outcome, PointerInput, PointerOwnership, PointerPhase, Scroll},
    DoubleClick, wheel,
};

/// What a turn of a stepping surface amounts to. `By` is a delta, not a value:
/// the surface never knows what it is stepping.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum StepEvent {
    By(f32),
    Activate,
}

/// A surface that steps a value by detents, by trackpad travel and by dragging.
/// It owns no configuration, so the gesture and its state are one value.
#[derive(Default)]
pub struct Stepper {
    double_click: DoubleClick,
    drag: Option<f32>,
    last_step: Option<Instant>,
}

impl Stepper {
    const DRAG_STEPS_PER_PIXEL: f32 = 0.25;
    /// How long one trackpad gesture owns the surface. A flick arrives as a
    /// long tail of shrinking pixel deltas, and every one of them would be its
    /// own detent without this.
    const STEP_INTERVAL_MS: u128 = 200;

    fn drag_steps(&mut self, y: f32) -> Option<f32> {
        let from = self.drag?;
        self.drag = Some(y);
        Some((from - y) * Self::DRAG_STEPS_PER_PIXEL)
    }

    #[must_use]
    pub const fn dragging(&self) -> bool {
        self.drag.is_some()
    }

    /// Measures travel against the event position, never the hit, since a host that expresses the
    /// hit locally puts the two in different coordinate spaces; mixing them would jump by the
    /// surface's offset from the window corner.
    pub fn on_input(&mut self, input: Input<'_>, hit: &Hit, now: Instant) -> Outcome<StepEvent> {
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
                self.drag = Some(pointer.at.unwrap_or(position).y);
                Outcome::IGNORED.with_ownership(PointerOwnership::Claim)
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
            Input::Pointer(pointer)
                if matches!(pointer.phase, PointerPhase::Up | PointerPhase::Cancel) =>
            {
                if self.drag.take().is_some() {
                    Outcome::captured().with_ownership(PointerOwnership::Release)
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
