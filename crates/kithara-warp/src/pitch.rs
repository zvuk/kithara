use kithara_derive::Ranged;

/// Semitones a pitch shifts by apart from speed, an octave at most either way.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Ranged)]
#[ranged(min = -12, max = 12, default = 0)]
pub struct Semitones(i8);

impl Semitones {
    /// One semitone up, held at [`Self::MAX`].
    #[must_use]
    pub const fn step_up(self) -> Self {
        if self.0 < Self::MAX.0 {
            Self(self.0 + 1)
        } else {
            self
        }
    }

    /// One semitone down, held at [`Self::MIN`].
    #[must_use]
    pub const fn step_down(self) -> Self {
        if self.0 > Self::MIN.0 {
            Self(self.0 - 1)
        } else {
            self
        }
    }
}

/// How a pitch follows speed: riding on it, or held against it by keylock,
/// shifted by semitones either way.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pitch {
    /// The pitch moves with the speed.
    Vinyl { shift: Semitones },
    /// The pitch stays put at any speed.
    Held { shift: Semitones },
}

/// A total transition of a [`Pitch`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PitchStep {
    /// One semitone up, held at the top of the range.
    Up,
    /// One semitone down, held at the bottom of the range.
    Down,
    /// Back to no shift, keylock as it was.
    Reset,
    /// Keylock switched, the shift kept.
    ToggleKeylock,
}

impl Default for Pitch {
    fn default() -> Self {
        Self::Vinyl {
            shift: Semitones::DEFAULT,
        }
    }
}

impl Pitch {
    /// Held by keylock with no shift.
    pub const HELD: Self = Self::Held {
        shift: Semitones::DEFAULT,
    };

    /// This pitch moved by `step`.
    #[must_use]
    pub const fn step(self, step: PitchStep) -> Self {
        match step {
            PitchStep::Up => self.with_shift(self.shift().step_up()),
            PitchStep::Down => self.with_shift(self.shift().step_down()),
            PitchStep::Reset => self.with_shift(Semitones::DEFAULT),
            PitchStep::ToggleKeylock => match self {
                Self::Vinyl { shift } => Self::Held { shift },
                Self::Held { shift } => Self::Vinyl { shift },
            },
        }
    }

    /// Whether keylock holds the pitch against speed.
    #[must_use]
    pub const fn keylock(self) -> bool {
        matches!(self, Self::Held { .. })
    }

    /// The pitch factor for source advancing `speed` frames per output frame.
    #[must_use]
    pub fn factor(self, speed: f64) -> f64 {
        let shift = (f64::from(self.shift().0) / 12.0).exp2();
        if self.keylock() { shift } else { speed * shift }
    }

    pub(crate) const fn shift(self) -> Semitones {
        match self {
            Self::Vinyl { shift } | Self::Held { shift } => shift,
        }
    }

    pub(crate) const fn with_shift(self, shift: Semitones) -> Self {
        match self {
            Self::Vinyl { .. } => Self::Vinyl { shift },
            Self::Held { .. } => Self::Held { shift },
        }
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    fn semitones(value: i8) -> Semitones {
        Semitones::checked(value).expect("fixture shift is in range")
    }

    /// A shift stays inside an octave: a step holds at either end and a value
    /// beyond it is unrepresentable.
    #[kithara::test]
    fn a_shift_stays_inside_an_octave() {
        assert_eq!(Semitones::DEFAULT.step_up(), semitones(1));
        assert_eq!(semitones(-3).step_down(), semitones(-4));
        assert_eq!(Semitones::MAX.step_up(), Semitones::MAX);
        assert_eq!(Semitones::MIN.step_down(), Semitones::MIN);
        for value in [13, -13] {
            assert_eq!(Semitones::checked(value), None);
            assert!(serde_json::from_str::<Semitones>(&value.to_string()).is_err());
        }
    }

    #[kithara::test]
    #[case::lock_keeps_the_shift(
        Pitch::Vinyl { shift: semitones(5) }.step(PitchStep::ToggleKeylock),
        Pitch::Held { shift: semitones(5) }
    )]
    #[case::up(
        Pitch::Held { shift: semitones(2) }.step(PitchStep::Up),
        Pitch::Held { shift: semitones(3) }
    )]
    #[case::reset_keeps_the_lock(
        Pitch::Vinyl { shift: semitones(-4) }.step(PitchStep::Reset),
        Pitch::Vinyl { shift: Semitones::DEFAULT }
    )]
    #[case::unlock_keeps_the_shift(
        Pitch::Held { shift: semitones(6) }.step(PitchStep::ToggleKeylock),
        Pitch::Vinyl { shift: semitones(6) }
    )]
    fn a_transition_moves_the_pitch(#[case] moved: Pitch, #[case] expected: Pitch) {
        assert_eq!(moved, expected);
    }
}
