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
}
