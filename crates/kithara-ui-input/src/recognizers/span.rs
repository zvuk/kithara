use kithara_ui_draw::{Pt, Rect};

use super::super::{CursorShape, Hit, Hover, Input, Outcome, PointerPhase};

/// Which end of a two-handled interval a gesture drives.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Edge {
    Min,
    Max,
}

/// A drag over an interval with a handle at each end.
///
/// The press picks the nearer handle and the gesture keeps it until release.
/// Re-deciding on every move would hand the drag to the other handle the moment
/// the pointer crossed it, and the interval would fold through itself instead
/// of being pushed. A tie goes to the lower handle, so the interval opens
/// downward from a press exactly between the two.
///
/// The handles are named rather than counted because each publishes under its
/// own endpoint; a numbered handle would make the host translate an index back
/// into a name it already had.
#[derive(Clone, Copy)]
pub struct Span {
    hover: Hover,
    max: f32,
    min: f32,
}

#[derive(Default, fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub struct SpanState {
    held: Option<Edge>,
}

impl SpanState {
    pub fn cancel_pointer(&mut self) {
        self.held = None;
    }

    #[must_use]
    pub const fn captures_pointer(&self) -> bool {
        self.held.is_some()
    }
}

impl Span {
    #[must_use]
    pub const fn new(hover: Hover, min: f32, max: f32) -> Self {
        Self { hover, max, min }
    }

    #[must_use]
    pub fn cursor(&self, state: &SpanState, hit: &Hit) -> CursorShape {
        self.hover.cursor(state.captures_pointer(), hit)
    }

    fn nearest(&self, value: f32) -> Edge {
        if (value - self.min).abs() <= (value - self.max).abs() {
            Edge::Min
        } else {
            Edge::Max
        }
    }

    pub fn on_input(
        &self,
        state: &mut SpanState,
        input: Input<'_>,
        hit: &Hit,
    ) -> Outcome<(Edge, f32)> {
        match input {
            Input::Pointer(pointer) if pointer.phase == PointerPhase::Down => {
                let Some(position) = hit.inside() else {
                    return Outcome::IGNORED;
                };
                let Some(value) = across(position, hit.area()) else {
                    return Outcome::IGNORED;
                };
                let edge = self.nearest(value);
                state.held = Some(edge);
                Outcome::set((edge, value))
            }
            Input::Pointer(pointer) if pointer.phase == PointerPhase::Move => {
                let Some(edge) = state.held else {
                    return Outcome::IGNORED;
                };
                hit.at()
                    .and_then(|position| across(position, hit.area()))
                    .map_or_else(Outcome::captured, |value| Outcome::set((edge, value)))
            }
            Input::Pointer(pointer)
                if matches!(
                    pointer.phase,
                    PointerPhase::Cancel | PointerPhase::Leave | PointerPhase::Up
                ) && state.held.is_some() =>
            {
                state.held = None;
                Outcome::captured()
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

/// The position read across the box, or nothing at all when the box has no
/// width: a control laid out to zero pixels has no value to report, and a
/// clamped zero would be a value the hand never asked for.
fn across(position: Pt, area: Rect) -> Option<f32> {
    (area.w > 0.0).then(|| ((position.x - area.x) / area.w).clamp(0.0, 1.0))
}
