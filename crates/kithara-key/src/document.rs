use serde::{Deserialize, Serialize};

use crate::key::{Mode, MusicalKey, PitchClass};

/// The key document schema this build reads and writes.
pub const KEY_SCHEMA_VERSION: u32 = 1;

/// A key document as it arrives over the wire, before any check.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RawKey {
    pub schema_version: u32,
    /// Semitones above C.
    pub tonic: u8,
    pub mode: Mode,
}

/// Why a key document names no key.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum KeyDocumentError {
    #[error("schema {found} is not the schema {expected} this build reads")]
    Schema { expected: u32, found: u32 },
    #[error("tonic {0} is past the 12 semitones above C")]
    Tonic(u8),
}

impl From<MusicalKey> for RawKey {
    fn from(key: MusicalKey) -> Self {
        Self {
            schema_version: KEY_SCHEMA_VERSION,
            tonic: key.tonic.semitone(),
            mode: key.mode,
        }
    }
}

impl TryFrom<RawKey> for MusicalKey {
    type Error = KeyDocumentError;

    fn try_from(raw: RawKey) -> Result<Self, Self::Error> {
        if raw.schema_version != KEY_SCHEMA_VERSION {
            return Err(KeyDocumentError::Schema {
                expected: KEY_SCHEMA_VERSION,
                found: raw.schema_version,
            });
        }
        let tonic = PitchClass::try_from(raw.tonic).map_err(KeyDocumentError::Tonic)?;
        Ok(Self {
            mode: raw.mode,
            tonic,
        })
    }
}
