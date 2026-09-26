/// Whether the current event may continue to content behind this component.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Propagation {
    /// Leaves the current event available to content behind the component.
    #[default]
    Ignored,
    /// Stops the current event at this component.
    Captured,
}

/// Requested change to retained pointer ownership after this event.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum PointerOwnership {
    /// Keeps the retained owner unchanged.
    #[default]
    Unchanged,
    /// Makes this component the retained owner of the current pointer. A
    /// component requests this while handling [`PointerPhase::Down`].
    ///
    /// [`PointerPhase::Down`]: crate::PointerPhase::Down
    Claim,
    /// Gives the current pointer back to hit-tested routing.
    Release,
}

/// Typed event value, propagation verdict, and retained pointer ownership.
#[derive(Clone, Copy, Debug, PartialEq, Eq, fieldwork::Fieldwork)]
#[fieldwork(opt_in, get, with)]
pub struct Outcome<T = f32> {
    value: Option<T>,
    #[field(get(copy), with)]
    ownership: PointerOwnership,
    #[field(get(copy))]
    propagation: Propagation,
}

impl<T> Outcome<T> {
    /// An event that neither produced a value nor stopped propagation.
    pub const IGNORED: Self = Self {
        value: None,
        propagation: Propagation::Ignored,
        ownership: PointerOwnership::Unchanged,
    };

    /// Stops this event without producing a value or changing pointer ownership.
    #[must_use]
    pub const fn captured() -> Self {
        Self {
            value: None,
            propagation: Propagation::Captured,
            ownership: PointerOwnership::Unchanged,
        }
    }

    /// Reports whether this event stops at the component.
    #[must_use]
    pub const fn is_captured(&self) -> bool {
        matches!(self.propagation, Propagation::Captured)
    }

    /// Maps the typed value without changing propagation or ownership.
    #[must_use]
    pub fn map<U, F>(self, map: F) -> Outcome<U>
    where
        F: FnOnce(T) -> U,
    {
        Outcome {
            value: self.value.map(map),
            propagation: self.propagation,
            ownership: self.ownership,
        }
    }

    /// Produces a typed value while allowing this event to continue and leaving
    /// pointer ownership unchanged.
    #[must_use]
    pub const fn observed(value: T) -> Self {
        Self {
            value: Some(value),
            propagation: Propagation::Ignored,
            ownership: PointerOwnership::Unchanged,
        }
    }

    /// Produces a typed value and stops this event without changing pointer ownership.
    #[must_use]
    pub const fn set(value: T) -> Self {
        Self {
            value: Some(value),
            propagation: Propagation::Captured,
            ownership: PointerOwnership::Unchanged,
        }
    }

    /// Takes the typed value, if the component produced one.
    #[must_use]
    pub fn value(self) -> Option<T> {
        self.value
    }
}
