/// One of the twelve pitch classes, counted in semitones up from C.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PitchClass {
    C,
    CSharp,
    D,
    DSharp,
    E,
    F,
    FSharp,
    G,
    GSharp,
    A,
    ASharp,
    B,
}

/// The scale of a key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Mode {
    Major,
    Minor,
}

/// One of the 24 keys: a tonic and a mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MusicalKey {
    pub mode: Mode,
    pub tonic: PitchClass,
}

/// A key and its strength: the correlation of the summed pitch-class profile
/// with that key's profile, at most 1.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct KeyEstimate {
    key: MusicalKey,
    strength: f32,
}

impl PitchClass {
    const ALL: [Self; 12] = [
        Self::C,
        Self::CSharp,
        Self::D,
        Self::DSharp,
        Self::E,
        Self::F,
        Self::FSharp,
        Self::G,
        Self::GSharp,
        Self::A,
        Self::ASharp,
        Self::B,
    ];

    /// Semitones above C, `0..12`.
    #[must_use]
    pub const fn semitone(self) -> u8 {
        self as u8
    }
}

/// The pitch class `semitone` semitones above C; past `11` the semitone comes
/// back as the error.
impl TryFrom<u8> for PitchClass {
    type Error = u8;

    fn try_from(semitone: u8) -> Result<Self, Self::Error> {
        Self::ALL
            .get(usize::from(semitone))
            .copied()
            .ok_or(semitone)
    }
}

impl MusicalKey {
    pub(crate) fn at_rotation(semitones_above_a: usize, mode: Mode) -> Self {
        let semitone = semitones_above_a + usize::from(PitchClass::A.semitone());
        Self {
            mode,
            tonic: PitchClass::ALL[semitone % PitchClass::ALL.len()],
        }
    }
}

impl KeyEstimate {
    /// `key` at `strength`, when the strength is finite and at most 1.
    #[must_use]
    pub fn new(key: MusicalKey, strength: f32) -> Option<Self> {
        (strength.is_finite() && strength <= 1.0).then_some(Self { key, strength })
    }

    #[must_use]
    pub const fn key(&self) -> MusicalKey {
        self.key
    }

    #[must_use]
    pub const fn strength(&self) -> f32 {
        self.strength
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn an_estimate_holds_a_finite_strength_of_at_most_one() {
        let key = MusicalKey {
            tonic: PitchClass::A,
            mode: Mode::Minor,
        };
        assert!(KeyEstimate::new(key, 1.0).is_some());
        assert!(KeyEstimate::new(key, -0.5).is_some());
        for strength in [1.5, f32::NAN, f32::INFINITY] {
            assert_eq!(KeyEstimate::new(key, strength), None, "{strength}");
        }
    }

    #[kithara::test]
    fn a_semitone_names_its_pitch_class_and_back() {
        for semitone in 0..12 {
            assert_eq!(
                PitchClass::try_from(semitone).map(PitchClass::semitone),
                Ok(semitone)
            );
        }
        assert_eq!(PitchClass::try_from(12), Err(12));
    }

    #[kithara::test]
    fn the_24_rotations_name_the_24_keys_from_a() {
        let mut keys = HashSet::new();
        for mode in [Mode::Major, Mode::Minor] {
            for rotation in 0..12 {
                let key = MusicalKey::at_rotation(rotation, mode);
                assert_eq!(key.mode, mode);
                assert_eq!(usize::from(key.tonic.semitone()), (rotation + 9) % 12);
                keys.insert(key);
            }
        }
        assert_eq!(keys.len(), 24);
        assert_eq!(MusicalKey::at_rotation(0, Mode::Minor).tonic, PitchClass::A);
        assert_eq!(MusicalKey::at_rotation(3, Mode::Major).tonic, PitchClass::C);
    }
}
