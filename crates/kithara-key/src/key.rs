use serde::{Deserialize, Serialize};

use crate::document::RawKey;

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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Major,
    Minor,
}

/// One of the 24 keys: a tonic and a mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(try_from = "RawKey", into = "RawKey")]
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
    /// Open Key label (`1d`..`12d` for major, `1m`..`12m` for minor) on the wheel of fifths.
    #[must_use]
    pub const fn open_key(self) -> &'static str {
        const MAJOR: [&str; 12] = [
            "1d", "2d", "3d", "4d", "5d", "6d", "7d", "8d", "9d", "10d", "11d", "12d",
        ];
        const MINOR: [&str; 12] = [
            "1m", "2m", "3m", "4m", "5m", "6m", "7m", "8m", "9m", "10m", "11m", "12m",
        ];
        let labels = match self.mode {
            Mode::Major => &MAJOR,
            Mode::Minor => &MINOR,
        };
        labels[self.open_key_index()]
    }

    /// Zero-based position of this key on the Open Key wheel, from C major or A minor.
    #[must_use]
    pub const fn open_key_index(self) -> usize {
        const FIFTH: usize = 7;
        const RELATIVE_MAJOR: usize = 3;
        let tonic = self.tonic.semitone() as usize;
        let major = match self.mode {
            Mode::Major => tonic,
            Mode::Minor => tonic + RELATIVE_MAJOR,
        };
        FIFTH * major % 12
    }

    /// This key moved by `semitones`, keeping its mode.
    #[must_use]
    pub fn transposed(self, semitones: i8) -> Self {
        let step = semitones.rem_euclid(12).unsigned_abs();
        let tonic = PitchClass::ALL[usize::from((self.tonic.semitone() + step) % 12)];
        Self {
            mode: self.mode,
            tonic,
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
    use kithara_test_utils::kithara;

    use super::*;
    use crate::document::{KEY_SCHEMA_VERSION, KeyDocumentError, RawKey};

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

    fn every_key() -> impl Iterator<Item = MusicalKey> {
        [Mode::Major, Mode::Minor].into_iter().flat_map(|mode| {
            PitchClass::ALL
                .into_iter()
                .map(move |tonic| MusicalKey { mode, tonic })
        })
    }

    #[kithara::test]
    fn every_key_reads_back_from_its_document() {
        for key in every_key() {
            let document = serde_json::to_string(&key).unwrap_or_else(|error| panic!("{error}"));
            let read: MusicalKey =
                serde_json::from_str(&document).unwrap_or_else(|error| panic!("{error}"));
            assert_eq!(read, key, "{document}");
        }
        assert_eq!(every_key().count(), 24);
    }

    #[kithara::test]
    fn tonic_nine_in_minor_reads_as_a_minor() {
        let document =
            format!(r#"{{"schema_version":{KEY_SCHEMA_VERSION},"tonic":9,"mode":"minor"}}"#);
        let key: MusicalKey =
            serde_json::from_str(&document).unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(
            key,
            MusicalKey {
                tonic: PitchClass::A,
                mode: Mode::Minor,
            }
        );
    }

    #[kithara::test]
    fn a_document_of_another_schema_is_refused_with_both_versions() {
        let raw = RawKey {
            schema_version: KEY_SCHEMA_VERSION + 1,
            tonic: 9,
            mode: Mode::Minor,
        };
        assert_eq!(
            MusicalKey::try_from(raw),
            Err(KeyDocumentError::Schema {
                expected: KEY_SCHEMA_VERSION,
                found: KEY_SCHEMA_VERSION + 1,
            })
        );
    }

    #[kithara::test]
    fn a_tonic_past_eleven_is_refused_with_its_value() {
        let raw = RawKey {
            schema_version: KEY_SCHEMA_VERSION,
            tonic: 12,
            mode: Mode::Major,
        };
        assert_eq!(MusicalKey::try_from(raw), Err(KeyDocumentError::Tonic(12)));
        let document =
            format!(r#"{{"schema_version":{KEY_SCHEMA_VERSION},"tonic":12,"mode":"major"}}"#);
        assert!(serde_json::from_str::<MusicalKey>(&document).is_err());
    }

    #[kithara::test]
    fn transposing_rotates_the_tonic_and_keeps_the_mode() {
        let c_major = MusicalKey {
            tonic: PitchClass::C,
            mode: Mode::Major,
        };
        let a_minor = MusicalKey {
            tonic: PitchClass::A,
            mode: Mode::Minor,
        };
        assert_eq!(
            c_major.transposed(1),
            MusicalKey {
                tonic: PitchClass::CSharp,
                mode: Mode::Major,
            }
        );
        assert_eq!(
            c_major.transposed(-1),
            MusicalKey {
                tonic: PitchClass::B,
                mode: Mode::Major,
            }
        );
        assert_eq!(a_minor.transposed(-12), a_minor);
        assert_eq!(
            c_major.transposed(i8::MIN),
            MusicalKey {
                tonic: PitchClass::E,
                mode: Mode::Major,
            }
        );
        assert_eq!(
            c_major.transposed(i8::MAX),
            MusicalKey {
                tonic: PitchClass::G,
                mode: Mode::Major,
            }
        );
        for key in every_key() {
            assert_eq!(key.transposed(0), key);
            assert_eq!(key.transposed(12), key);
            assert_eq!(key.transposed(-12), key);
        }
    }

    #[kithara::test]
    fn the_24_keys_name_their_open_key_numbers() {
        let wheel = [
            ("1d", PitchClass::C, Mode::Major),
            ("1m", PitchClass::A, Mode::Minor),
            ("2d", PitchClass::G, Mode::Major),
            ("2m", PitchClass::E, Mode::Minor),
            ("3d", PitchClass::D, Mode::Major),
            ("3m", PitchClass::B, Mode::Minor),
            ("4d", PitchClass::A, Mode::Major),
            ("4m", PitchClass::FSharp, Mode::Minor),
            ("5d", PitchClass::E, Mode::Major),
            ("5m", PitchClass::CSharp, Mode::Minor),
            ("6d", PitchClass::B, Mode::Major),
            ("6m", PitchClass::GSharp, Mode::Minor),
            ("7d", PitchClass::FSharp, Mode::Major),
            ("7m", PitchClass::DSharp, Mode::Minor),
            ("8d", PitchClass::CSharp, Mode::Major),
            ("8m", PitchClass::ASharp, Mode::Minor),
            ("9d", PitchClass::GSharp, Mode::Major),
            ("9m", PitchClass::F, Mode::Minor),
            ("10d", PitchClass::DSharp, Mode::Major),
            ("10m", PitchClass::C, Mode::Minor),
            ("11d", PitchClass::ASharp, Mode::Major),
            ("11m", PitchClass::G, Mode::Minor),
            ("12d", PitchClass::F, Mode::Major),
            ("12m", PitchClass::D, Mode::Minor),
        ];
        for (label, tonic, mode) in wheel {
            assert_eq!(MusicalKey { mode, tonic }.open_key(), label);
        }
    }
}
