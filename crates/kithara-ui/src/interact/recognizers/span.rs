use super::super::{CursorShape, Hit, Hover, Input, Outcome, PointerPhase};
use crate::draw::{Pt, Rect};

/// Which end of a two-handled interval a gesture drives.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Edge {
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
pub(crate) struct Span {
    hover: Hover,
    max: f32,
    min: f32,
}

#[derive(Default, fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub(crate) struct SpanState {
    held: Option<Edge>,
}

impl SpanState {
    pub(crate) const fn captures_pointer(&self) -> bool {
        self.held.is_some()
    }

    #[cfg(feature = "masonry")]
    pub(crate) fn cancel_pointer(&mut self) {
        self.held = None;
    }
}

impl Span {
    pub(crate) const fn new(hover: Hover, min: f32, max: f32) -> Self {
        Self { hover, max, min }
    }

    pub(crate) fn on_input(
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

    pub(crate) fn cursor(&self, state: &SpanState, hit: &Hit) -> CursorShape {
        self.hover.cursor(state.captures_pointer(), hit)
    }

    fn nearest(&self, value: f32) -> Edge {
        if (value - self.min).abs() <= (value - self.max).abs() {
            Edge::Min
        } else {
            Edge::Max
        }
    }
}

/// The position read across the box, or nothing at all when the box has no
/// width: a control laid out to zero pixels has no value to report, and a
/// clamped zero would be a value the hand never asked for.
fn across(position: Pt, area: Rect) -> Option<f32> {
    (area.w > 0.0).then(|| ((position.x - area.x) / area.w).clamp(0.0, 1.0))
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::{
        super::super::{PointerInput, mouse},
        *,
    };

    const AREA: Rect = Rect {
        h: 20.0,
        w: 200.0,
        x: 10.0,
        y: 0.0,
    };

    fn span() -> Span {
        Span::new(Hover::new(CursorShape::ResizeH), 0.2, 0.8)
    }

    fn hit(x: f32) -> Hit {
        Hit::new(Some(Pt { x, y: 10.0 }), AREA)
    }

    fn press(x: f32) -> PointerInput {
        mouse(PointerPhase::Down, Some(Pt { x, y: 10.0 }))
    }

    fn moved(x: f32) -> PointerInput {
        mouse(PointerPhase::Move, Some(Pt { x, y: 10.0 }))
    }

    /// The press picks the handle it lands nearer to, which is the whole reason
    /// one control can write two endpoints.
    #[kithara::test]
    fn a_press_takes_the_nearer_handle() {
        let span = span();

        let mut low = SpanState::default();
        assert_eq!(
            span.on_input(&mut low, Input::Pointer(press(50.0)), &hit(50.0))
                .value()
                .map(|(edge, _)| edge),
            Some(Edge::Min)
        );

        let mut high = SpanState::default();
        assert_eq!(
            span.on_input(&mut high, Input::Pointer(press(190.0)), &hit(190.0))
                .value()
                .map(|(edge, _)| edge),
            Some(Edge::Max)
        );
    }

    /// A drag that crosses the other handle keeps writing the endpoint it
    /// started on; swapping mid-gesture would fold the interval through itself.
    #[kithara::test]
    fn a_held_handle_survives_crossing_the_other_one() {
        let span = span();
        let mut state = SpanState::default();
        span.on_input(&mut state, Input::Pointer(press(50.0)), &hit(50.0));

        let outcome = span.on_input(&mut state, Input::Pointer(moved(200.0)), &hit(200.0));

        assert_eq!(outcome.value(), Some((Edge::Min, 0.95)));
    }

    /// Release gives the pointer back, so the next press picks a handle again.
    #[kithara::test]
    fn release_ends_the_gesture() {
        let span = span();
        let mut state = SpanState::default();
        span.on_input(&mut state, Input::Pointer(press(190.0)), &hit(190.0));
        assert!(state.captures_pointer());

        span.on_input(
            &mut state,
            Input::Pointer(mouse(PointerPhase::Up, Some(Pt { x: 190.0, y: 10.0 }))),
            &hit(190.0),
        );

        assert!(!state.captures_pointer());
        assert_eq!(
            span.on_input(&mut state, Input::Pointer(moved(50.0)), &hit(50.0))
                .value(),
            None,
            "a move after release belongs to nobody"
        );
    }

    /// A control laid out to nothing has no position under the pointer at all —
    /// `Rect::contains` is half-open, so a zero-width box holds no point. The
    /// press is therefore not this control's to take, and leaving it uncaptured
    /// is what lets whatever is behind it answer.
    #[kithara::test]
    fn a_degenerate_box_takes_no_press() {
        let span = span();
        let mut state = SpanState::default();
        let flat = Hit::new(Some(Pt { x: 10.0, y: 10.0 }), Rect { w: 0.0, ..AREA });

        let outcome = span.on_input(&mut state, Input::Pointer(press(10.0)), &flat);

        assert_eq!(outcome.value(), None);
        assert!(!outcome.is_captured());
        assert!(!state.captures_pointer());
    }
}
