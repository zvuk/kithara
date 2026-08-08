use crate::{
    engine::scalar_value,
    interact::{
        CursorShape, Hover, Input,
        recognizers::{Scalar, Track, WheelStep},
    },
    render::UiEvent,
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
    /// A press that picks one of a row of equal cells.
    Index { count: usize },
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

    /// The same drag counting from the value the control now draws. Only a
    /// host that keeps its widgets needs this; the other builds a fresh drag
    /// with every frame.
    #[cfg(feature = "masonry-host")]
    pub(crate) fn at(self, value: f32) -> Self {
        Self {
            track: self.track.at(value),
            wheel: self.wheel.map(|wheel| WheelStep { value, ..wheel }),
            ..self
        }
    }
}
