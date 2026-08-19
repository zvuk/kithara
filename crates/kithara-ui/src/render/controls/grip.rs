#[cfg(test)]
use crate::interact::Gestures;
#[cfg(feature = "masonry")]
use crate::interact::recognizers::Edge;
use crate::{
    atoms::painter::IndexedVisual,
    engine::scalar_value,
    interact::{
        CursorShape, Hover, Input, Outcome, PointerOwnership, PointerPhase, recognizers,
        recognizers::{Scalar, Track, WheelStep},
    },
    render::{ControlAction, ScalarRange, UiEvent, control_event},
};

/// What the pointer means to a control.
#[derive(Clone, Copy)]
pub(crate) enum Grip {
    /// Nothing the control itself recognises: either it is not interactive, or
    /// the engine plan drives it.
    None,
    /// A press that activates it.
    Press,
    /// A press that says something to the document rather than setting the
    /// control's own endpoint. The settings button opens a surface the
    /// application owns; there is no endpoint under it to activate.
    ///
    /// The event is named by a function rather than carried, so a grip stays
    /// `Copy` and a control that builds one every frame allocates nothing.
    Command(fn() -> UiEvent),
    /// A drag along one axis that sets a scalar.
    Drag(Drag),
    /// A press that picks one indexed cell. Painters use equal horizontal cells
    /// by default and may narrow the hit geometry to what they actually draw.
    Index { count: usize },
    /// A drag over an interval, setting whichever of its two ends the press
    /// landed nearer to.
    Span(Span),
}

/// A two-handled interval drag, described rather than built.
///
/// The description carries the interval itself because the press has to know
/// where the two handles currently are to pick one. A host that rebuilds its
/// tree every frame gets that for free; a host that keeps its widgets is told
/// the new interval through [`Self::at`].
#[derive(Clone, Copy, bon::Builder)]
pub(crate) struct Span {
    cursor: CursorShape,
    value: ScalarRange,
}

impl Span {
    pub(crate) const fn recognizer(self) -> recognizers::Span {
        recognizers::Span::new(Hover::new(self.cursor), self.value.min, self.value.max)
    }

    /// The interval that results from moving one of its ends.
    ///
    /// The other end is left exactly where it was, and the two are not ordered:
    /// snapping and the minimum gap between them belong to the host, and a
    /// control that closed the gap itself would fight the answer coming back.
    #[cfg(feature = "masonry")]
    pub(crate) const fn moved(self, edge: Edge, value: f32) -> ScalarRange {
        match edge {
            Edge::Min => ScalarRange {
                min: value,
                max: self.value.max,
            },
            Edge::Max => ScalarRange {
                min: self.value.min,
                max: value,
            },
        }
    }

    /// The same drag measured against the interval the control now draws.
    #[cfg(feature = "masonry")]
    pub(crate) const fn at(self, value: ScalarRange) -> Self {
        Self { value, ..self }
    }
}

/// A scalar drag, described rather than built.
///
/// A host that rebuilds its tree every frame could hold the recognizer itself,
/// because the value it counts from is fresh each time. A host that keeps its
/// widgets cannot: it is told the new value instead, and has to re-make the
/// recognizer from it — which it can only do from the description.
#[derive(Clone, Copy, bon::Builder)]
pub(crate) struct Drag {
    cursor: CursorShape,
    track: Track,
    reset: Option<f32>,
    /// What the published value is rounded to while the hand is on it. A fader
    /// walks in steps the skin names; every other control publishes what the
    /// pointer says.
    step: Option<f64>,
    wheel: Option<WheelStep>,
}

impl Drag {
    pub(crate) fn recognizer(self) -> Scalar {
        Scalar::builder()
            .track(self.track)
            .hover(Hover::new(self.cursor))
            .maybe_reset(self.reset)
            .maybe_wheel(self.wheel)
            .build()
    }

    /// What this drag publishes for a value the recognizer produced.
    pub(crate) fn published(self, input: Input<'_>, value: f32) -> f64 {
        scalar_value(input, value, self.step)
    }

    #[cfg(test)]
    pub(crate) const fn gestures(self) -> Gestures {
        Gestures {
            double_click: self.reset.is_some(),
            drag: true,
            keyboard: false,
            press: true,
            wheel: self.wheel.is_some(),
        }
    }

    /// The same drag counting from the value the control now draws. Only a
    /// host that keeps its widgets needs this; the other builds a fresh drag
    /// with every frame.
    #[cfg(feature = "masonry")]
    pub(crate) fn at(self, value: f32) -> Self {
        Self {
            track: self.track.at(value),
            wheel: self.wheel.map(|wheel| WheelStep { value, ..wheel }),
            ..self
        }
    }
}

pub(crate) type IndexEvent<Data> = fn(&Data, usize) -> Option<UiEvent>;

pub(crate) struct Indexing<'a, Data> {
    data: &'a Data,
    map: Option<IndexEvent<Data>>,
    path: &'a str,
}

impl<'a, Data> Indexing<'a, Data> {
    pub(crate) const fn new(data: &'a Data, path: &'a str, map: Option<IndexEvent<Data>>) -> Self {
        Self { data, map, path }
    }

    pub(crate) fn on_input(
        &self,
        state: &mut IndexPress,
        input: Input<'_>,
        index: Option<usize>,
    ) -> (bool, Outcome<UiEvent>) {
        let (changed, selected) = state.follow(input, index, self.map.is_some());
        let captured = selected.is_captured();
        let ownership = selected.ownership();
        let event = selected.value().and_then(|index| {
            self.map.map_or_else(
                || Some(control_event(self.path, ControlAction::SelectIndex(index))),
                |map| map(self.data, index),
            )
        });
        match (event, captured) {
            (Some(event), true) => (changed, Outcome::set(event).with_ownership(ownership)),
            (Some(event), false) => (changed, Outcome::observed(event).with_ownership(ownership)),
            (None, true) => (changed, Outcome::captured().with_ownership(ownership)),
            (None, false) => (changed, Outcome::IGNORED.with_ownership(ownership)),
        }
    }
}

#[derive(Default)]
pub(crate) struct IndexPress {
    hovered: Option<usize>,
    pressed_origin: Option<usize>,
}

impl IndexPress {
    pub(crate) const fn visual(&self) -> IndexedVisual {
        IndexedVisual {
            hovered: self.hovered,
            pressed_origin: self.pressed_origin,
        }
    }

    fn follow(
        &mut self,
        input: Input<'_>,
        index: Option<usize>,
        release_activation: bool,
    ) -> (bool, Outcome<usize>) {
        let Input::Pointer(pointer) = input else {
            return (false, Outcome::IGNORED);
        };
        if !release_activation {
            let selected = match pointer.phase {
                PointerPhase::Down => index.map_or(Outcome::IGNORED, Outcome::set),
                PointerPhase::Cancel
                | PointerPhase::DoubleClick
                | PointerPhase::Leave
                | PointerPhase::LongPress
                | PointerPhase::Move
                | PointerPhase::MoveLongPress
                | PointerPhase::Up => Outcome::IGNORED,
            };
            return (false, selected);
        }
        let pressed_origin = self.pressed_origin;
        let hovered = match pointer.phase {
            PointerPhase::Cancel | PointerPhase::Leave => None,
            PointerPhase::Down
            | PointerPhase::LongPress
            | PointerPhase::Move
            | PointerPhase::MoveLongPress
            | PointerPhase::Up
            | PointerPhase::DoubleClick => index,
        };
        let next_pressed_origin = match pointer.phase {
            PointerPhase::Down => index,
            PointerPhase::Cancel
            | PointerPhase::DoubleClick
            | PointerPhase::Leave
            | PointerPhase::Up => None,
            PointerPhase::LongPress | PointerPhase::Move | PointerPhase::MoveLongPress => {
                pressed_origin
            }
        };
        let changed = self.hovered != hovered || self.pressed_origin != next_pressed_origin;
        self.hovered = hovered;
        self.pressed_origin = next_pressed_origin;
        let selected = match pointer.phase {
            PointerPhase::Down if index.is_some() => {
                Outcome::captured().with_ownership(PointerOwnership::Claim)
            }
            PointerPhase::Up => match pressed_origin {
                Some(origin) if index == Some(origin) => {
                    Outcome::set(origin).with_ownership(PointerOwnership::Release)
                }
                Some(_) => Outcome::captured().with_ownership(PointerOwnership::Release),
                None => Outcome::IGNORED,
            },
            PointerPhase::Cancel | PointerPhase::DoubleClick | PointerPhase::Leave
                if pressed_origin.is_some() =>
            {
                Outcome::captured().with_ownership(PointerOwnership::Release)
            }
            PointerPhase::LongPress | PointerPhase::Move | PointerPhase::MoveLongPress
                if pressed_origin.is_some() =>
            {
                Outcome::captured()
            }
            PointerPhase::Down
            | PointerPhase::Cancel
            | PointerPhase::DoubleClick
            | PointerPhase::Leave
            | PointerPhase::LongPress
            | PointerPhase::Move
            | PointerPhase::MoveLongPress => Outcome::IGNORED,
        };
        (changed, selected)
    }

    #[cfg(feature = "masonry")]
    pub(crate) fn hover(&mut self, hovered: bool) -> bool {
        if hovered || self.hovered.is_none() {
            return false;
        }
        self.hovered = None;
        true
    }
}
