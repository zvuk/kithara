use kithara_platform::time::Instant;
use kithara_ui_draw::{Pt, Rect};

use super::{
    super::{CursorShape, Hit, Hover, Input, Outcome, PointerInput, PointerPhase},
    DoubleClick, wheel,
};

/// How a pointer position becomes a value. A relative track counts travel from
/// the press, so the press only arms it; an absolute track reads the position
/// itself, so the press seeks straight there.
#[derive(Clone, Copy)]
pub enum Track {
    /// Vertical travel divided by `range` and added to `value`; up is positive.
    RelativeVertical { range: f32, value: f32 },
    /// Horizontal travel over the area's width, scaled and *subtracted* from
    /// `value`: the content moves with the pointer under a fixed playhead, so
    /// dragging right walks the position back.
    RelativeHorizontal { scale: f32, value: f32 },
    /// Horizontal travel added to `value` in pixels, floored at `minimum`. The
    /// only track whose value is a width rather than a fraction, which is why
    /// it has a floor and no ceiling.
    HorizontalPixels { minimum: f32, value: f32 },
    /// The position normalized against the area's height, bottom at zero.
    AbsoluteVertical,
    /// The position normalized against the area's width.
    AbsoluteHorizontal,
    /// The position normalized against the area's width, once: the press seeks
    /// and never arms, so the pointer stays free for whoever wants it next.
    HorizontalClick,
}

impl Track {
    const fn arms(self) -> bool {
        !matches!(self, Self::HorizontalClick)
    }

    /// The same track counting from a new value.
    ///
    /// A relative track starts from the value it was built with, so a host that
    /// keeps its widgets has to re-make it whenever the endpoint moves or the
    /// next drag walks the control back to where it mounted. An absolute track
    /// reads the position itself and has nothing to move; a pixel track's value
    /// is a width rather than a fraction, and is not what an endpoint reports.
    #[must_use]
    pub const fn at(self, value: f32) -> Self {
        match self {
            Self::RelativeVertical { range, .. } => Self::RelativeVertical { range, value },
            Self::RelativeHorizontal { scale, .. } => Self::RelativeHorizontal { scale, value },
            Self::HorizontalPixels { .. }
            | Self::AbsoluteVertical
            | Self::AbsoluteHorizontal
            | Self::HorizontalClick => self,
        }
    }
}

#[derive(bon::Builder)]
pub struct Scalar {
    hover: Hover,
    reset: Option<f32>,
    wheel: Option<WheelStep>,
    track: Track,
}

/// Opt-in wheel stepping: the current normalized value plus the per-tick step.
#[derive(Clone, Copy)]
pub struct WheelStep {
    pub step: f32,
    pub value: f32,
}

#[derive(Default, fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub struct ScalarState {
    double_click: DoubleClick,
    #[field(get = captures_pointer)]
    active: bool,
    start_position: f32,
    start_value: f32,
    wheel_accum: f32,
}

impl ScalarState {
    pub fn cancel_pointer(&mut self) {
        self.active = false;
    }
}

impl Scalar {
    #[must_use]
    pub fn cursor(&self, state: &ScalarState, hit: &Hit) -> CursorShape {
        self.hover.cursor(state.active, hit)
    }

    pub fn on_input(
        &self,
        state: &mut ScalarState,
        input: Input<'_>,
        hit: &Hit,
        now: Instant,
    ) -> Outcome {
        match input {
            Input::Pointer(pointer) if pointer.phase == PointerPhase::Down => {
                let Some(position) = hit.inside() else {
                    return Outcome::IGNORED;
                };
                if let Some(value) = self.reset
                    && state.double_click.register(position, now)
                {
                    state.active = false;
                    return Outcome::set(value);
                }
                let travel_position = pointer.at.unwrap_or(position);
                state.active = self.track.arms();
                match self.track {
                    Track::RelativeVertical { value, .. } => {
                        state.start_position = travel_position.y;
                        state.start_value = value;
                        Outcome::captured()
                    }
                    Track::RelativeHorizontal { value, .. }
                    | Track::HorizontalPixels { value, .. } => {
                        state.start_position = travel_position.x;
                        state.start_value = value;
                        Outcome::captured()
                    }
                    Track::AbsoluteVertical => seek_down(position, hit.area()),
                    Track::AbsoluteHorizontal | Track::HorizontalClick => {
                        seek_across(position, hit.area())
                    }
                }
            }
            Input::Pointer(PointerInput {
                phase: PointerPhase::Move,
                at: Some(at),
                ..
            }) if state.active => match self.track {
                Track::RelativeVertical { range, .. } => Outcome::set(
                    (state.start_value + (state.start_position - at.y) / range).clamp(0.0, 1.0),
                ),
                Track::RelativeHorizontal { scale, .. } => {
                    let width = hit.area().w;
                    if width > 0.0 {
                        Outcome::set(
                            (state.start_value - (at.x - state.start_position) / width * scale)
                                .clamp(0.0, 1.0),
                        )
                    } else {
                        Outcome::IGNORED
                    }
                }
                Track::HorizontalPixels { minimum, .. } => {
                    Outcome::set((state.start_value + at.x - state.start_position).max(minimum))
                }
                Track::AbsoluteVertical => hit
                    .at()
                    .map_or(Outcome::IGNORED, |position| seek_down(position, hit.area())),
                Track::AbsoluteHorizontal | Track::HorizontalClick => {
                    hit.at().map_or(Outcome::IGNORED, |position| {
                        seek_across(position, hit.area())
                    })
                }
            },
            Input::Pointer(pointer) if pointer.phase == PointerPhase::Up && state.active => {
                state.active = false;
                Outcome::captured()
            }
            Input::Wheel(scroll) if hit.over() => {
                let Some(wheel) = self.wheel else {
                    return Outcome::IGNORED;
                };
                let steps = wheel::steps(&mut state.wheel_accum, scroll);
                if steps == 0.0 {
                    return Outcome::captured();
                }
                let value = wheel.step.mul_add(steps, wheel.value);
                Outcome::set(value.clamp(0.0, 1.0))
            }
            Input::InputMethod(_)
            | Input::KeyPressed { .. }
            | Input::KeyReleased { .. }
            | Input::ModifiersChanged(_)
            | Input::Pointer(_)
            | Input::Wheel(_) => Outcome::IGNORED,
        }
    }
}

fn seek_down(position: Pt, area: Rect) -> Outcome {
    (area.h > 0.0)
        .then(|| (1.0 - (position.y - area.y) / area.h).clamp(0.0, 1.0))
        .map_or(Outcome::IGNORED, Outcome::set)
}

fn seek_across(position: Pt, area: Rect) -> Outcome {
    (area.w > 0.0)
        .then(|| ((position.x - area.x) / area.w).clamp(0.0, 1.0))
        .map_or(Outcome::IGNORED, Outcome::set)
}
