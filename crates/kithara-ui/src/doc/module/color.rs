use serde::{Deserialize, Serialize};

use super::binding::BindingRef;
use crate::skin::ColorRole;

/// A colour named directly from the palette, or supplied by a palette-role read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, derive_more::From)]
#[non_exhaustive]
pub enum ColorSource<B = BindingRef> {
    #[from]
    Role(ColorRole),
    FromRead {
        read: B,
        base: ColorRole,
    },
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
enum ColorFromRead {
    FromRead { read: BindingRef, base: ColorRole },
}

#[derive(Deserialize, Serialize)]
#[serde(untagged)]
enum ColorSourceRepr {
    Role(ColorRole),
    FromRead(ColorFromRead),
}

impl Serialize for ColorSource<BindingRef> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Self::Role(role) => ColorSourceRepr::Role(*role).serialize(serializer),
            Self::FromRead { read, base } => ColorSourceRepr::FromRead(ColorFromRead::FromRead {
                read: read.clone(),
                base: *base,
            })
            .serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for ColorSource<BindingRef> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        match ColorSourceRepr::deserialize(deserializer)? {
            ColorSourceRepr::Role(role) => Ok(Self::Role(role)),
            ColorSourceRepr::FromRead(ColorFromRead::FromRead { read, base }) => {
                Ok(Self::FromRead { read, base })
            }
        }
    }
}

impl<B> ColorSource<B> {
    /// Binding that drives this colour, when it comes from a read.
    #[must_use]
    pub const fn binding(&self) -> Option<&B> {
        match self {
            Self::Role(_) => None,
            Self::FromRead { read, .. } => Some(read),
        }
    }

    pub(crate) fn try_map<U, E>(
        &self,
        map: impl FnOnce(&B) -> Result<U, E>,
    ) -> Result<ColorSource<U>, E> {
        match self {
            Self::Role(role) => Ok(ColorSource::Role(*role)),
            Self::FromRead { read, base } => Ok(ColorSource::FromRead {
                read: map(read)?,
                base: *base,
            }),
        }
    }
}
