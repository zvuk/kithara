/// Whether the current event may continue to content behind this component.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub enum Propagation {
    /// Leaves the current event available to content behind the component.
    #[default]
    Ignored,
    /// Stops the current event at this component.
    Captured,
}

/// Requested change to retained pointer ownership after this event.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub enum PointerOwnership {
    /// Keeps the retained owner unchanged.
    #[default]
    Unchanged,
    /// Makes this component the retained owner of the current pointer. A
    /// component requests this while handling [`PointerPhase::Down`].
    ///
    /// [`PointerPhase::Down`]: crate::interact::PointerPhase::Down
    Claim,
    /// Gives the current pointer back to hit-tested routing.
    Release,
}

/// Typed event value, propagation verdict, and retained pointer ownership.
#[derive(Clone, Copy, Debug, PartialEq, fieldwork::Fieldwork)]
#[non_exhaustive]
#[fieldwork(opt_in, get, with)]
pub struct Outcome<T = f32> {
    value: Option<T>,
    #[field(get(copy), vis = "pub")]
    propagation: Propagation,
    #[field(get(copy), with, vis = "pub")]
    ownership: PointerOwnership,
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

    /// Produces a typed value and stops this event without changing pointer ownership.
    #[must_use]
    pub const fn set(value: T) -> Self {
        Self {
            value: Some(value),
            propagation: Propagation::Captured,
            ownership: PointerOwnership::Unchanged,
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

    /// Reports whether this event stops at the component.
    #[must_use]
    pub const fn is_captured(&self) -> bool {
        matches!(self.propagation, Propagation::Captured)
    }

    /// Takes the typed value, if the component produced one.
    #[must_use]
    pub fn value(self) -> Option<T> {
        self.value
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
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn propagation_and_retained_pointer_ownership_are_independent() {
        let claim = Outcome::<()>::captured().with_ownership(PointerOwnership::Claim);
        assert_eq!(claim.propagation(), Propagation::Captured);
        assert_eq!(claim.ownership(), PointerOwnership::Claim);

        let release = Outcome::observed(42).with_ownership(PointerOwnership::Release);
        assert_eq!(release.propagation(), Propagation::Ignored);
        assert_eq!(release.ownership(), PointerOwnership::Release);
        assert_eq!(release.value(), Some(42));
    }

    #[kithara::test]
    fn existing_outcome_constructors_leave_pointer_ownership_unchanged() {
        assert_eq!(
            Outcome::<()>::IGNORED.ownership(),
            PointerOwnership::Unchanged
        );
        assert_eq!(
            Outcome::<()>::captured().ownership(),
            PointerOwnership::Unchanged
        );
        assert_eq!(Outcome::set(3).ownership(), PointerOwnership::Unchanged);
        assert_eq!(
            Outcome::observed(3).ownership(),
            PointerOwnership::Unchanged
        );
    }
}
