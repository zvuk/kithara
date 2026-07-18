use serde::Deserialize;

use crate::{
    error::UiDocError,
    ids::{DocId, SourceUri},
    ron_io,
};

pub(crate) const LAYOUT_SCHEMA: &str = "kithara.layout";
pub(crate) const MODULE_SCHEMA: &str = "kithara.module";
pub const LAYOUT_VERSION: u32 = 1;
pub const MODULE_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DocKind {
    Layout,
    Module,
}

impl DocKind {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Layout => "layout",
            Self::Module => "module",
        }
    }
}

#[derive(Debug, Deserialize)]
struct EnvelopeProbe {
    schema: String,
    version: u32,
    id: DocId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct Envelope {
    pub kind: DocKind,
    pub version: u32,
    pub id: DocId,
}

/// Reads and validates a document envelope.
///
/// # Errors
/// Returns [`UiDocError`] when the RON, schema, or version is invalid.
pub fn probe(text: &str, origin: &SourceUri) -> Result<Envelope, UiDocError> {
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
    } else {
        return Err(UiDocError::UnknownSchema {
            origin: origin.clone(),
            schema: raw.schema,
        });
    };
    if raw.version == 0 || raw.version > max {
        return Err(UiDocError::UnsupportedVersion {
            origin: origin.clone(),
            schema: raw.schema,
            version: raw.version,
            max,
        });
    }
    Ok(Envelope {
        kind,
        version: raw.version,
        id: raw.id,
    })
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    fn origin() -> SourceUri {
        SourceUri("test.ron".into())
    }

    #[kithara::test]
    fn probes_layout_envelope_ignoring_body() {
        let env = probe(
            r#"(schema: "kithara.layout", version: 1, id: "micro", root: ())"#,
            &origin(),
        )
        .unwrap();
        assert_eq!(env.kind, DocKind::Layout);
        assert_eq!(env.version, 1);
        assert_eq!(env.id, DocId("micro".into()));
    }

    #[kithara::test]
    fn rejects_unknown_schema() {
        let err = probe(
            r#"(schema: "kithara.nope", version: 1, id: "x")"#,
            &origin(),
        )
        .unwrap_err();
        assert!(matches!(
            err,
            UiDocError::UnknownSchema { schema, .. } if schema == "kithara.nope"
        ));
    }

    #[kithara::test]
    fn rejects_future_version() {
        let err = probe(
            r#"(schema: "kithara.module", version: 99, id: "x")"#,
            &origin(),
        )
        .unwrap_err();
        assert!(matches!(
            err,
            UiDocError::UnsupportedVersion {
                version: 99,
                max: 1,
                ..
            }
        ));
    }

    #[kithara::test]
    fn syntax_error_carries_origin() {
        let err = probe("(((", &origin()).unwrap_err();
        assert!(err.to_string().starts_with("test.ron:"));
    }
}
