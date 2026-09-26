use serde::Deserialize;

use super::ron_io;
use crate::{
    error::UiDocError,
    ids::{DocId, SourceUri},
};

pub const LAYOUT_VERSION: u32 = 1;
pub const MODULE_VERSION: u32 = 1;
pub const PACKAGE_VERSION: u32 = 1;
pub const SKIN_VERSION: u32 = 1;
pub const TEXT_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DocKind {
    Layout,
    Module,
    Package,
    Skin,
    Text,
}

impl DocKind {
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Layout => "layout",
            Self::Module => "module",
            Self::Package => "package",
            Self::Skin => "skin",
            Self::Text => "text",
        }
    }
}

#[derive(Debug, Deserialize)]
struct EnvelopeProbe {
    id: DocId,
    schema: String,
    version: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct Envelope {
    pub id: DocId,
    pub kind: DocKind,
    pub version: u32,
}

/// Reads and validates a document envelope.
///
/// # Errors
/// Returns [`UiDocError`] when the RON, schema, or version is invalid.
pub fn probe(text: &str, origin: &SourceUri) -> Result<Envelope, UiDocError> {
    const LAYOUT_SCHEMA: &str = "kithara.layout";
    const MODULE_SCHEMA: &str = "kithara.module";
    const PACKAGE_SCHEMA: &str = "kithara.package";
    const SKIN_SCHEMA: &str = "kithara.skin";
    const TEXT_SCHEMA: &str = "kithara.text";

    let raw: EnvelopeProbe =
        ron_io::options()
            .from_str(text)
            .map_err(|source| UiDocError::Syntax {
                origin: origin.clone(),
                source: Box::new(source),
            })?;
    let (kind, max) = if raw.schema == LAYOUT_SCHEMA {
        (DocKind::Layout, LAYOUT_VERSION)
    } else if raw.schema == MODULE_SCHEMA {
        (DocKind::Module, MODULE_VERSION)
    } else if raw.schema == PACKAGE_SCHEMA {
        (DocKind::Package, PACKAGE_VERSION)
    } else if raw.schema == SKIN_SCHEMA {
        (DocKind::Skin, SKIN_VERSION)
    } else if raw.schema == TEXT_SCHEMA {
        (DocKind::Text, TEXT_VERSION)
    } else {
        return Err(UiDocError::UnknownSchema {
            origin: origin.clone(),
            schema: raw.schema,
        });
    };
    if raw.version == 0 || raw.version > max {
        return Err(UiDocError::UnsupportedVersion {
            max,
            origin: origin.clone(),
            schema: raw.schema,
            version: raw.version,
        });
    }
    Ok(Envelope {
        kind,
        version: raw.version,
        id: raw.id,
    })
}
