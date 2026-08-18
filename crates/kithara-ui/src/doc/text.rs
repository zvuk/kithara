use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::ron_io;
use crate::{
    envelope::{self, DocKind},
    error::UiDocError,
    ids::{DocId, SourceUri},
};

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct TextDoc {
    pub id: DocId,
    pub schema: String,
    pub version: u32,
    pub entries: BTreeMap<String, String>,
}

impl TextDoc {
    delegate::delegate! {
        to self.entries {
            #[must_use]
            #[expr($.map(String::as_str))]
            pub fn get(&self, key: &str) -> Option<&str>;
            #[expr($.map(String::as_str))]
            pub fn keys(&self) -> impl Iterator<Item = &str>;
        }
    }

    /// Combines this catalog with `other`.
    ///
    /// # Errors
    /// Returns [`UiDocError::DuplicateTextKey`] when a key is present in both catalogs.
    pub fn merge(&self, other: &Self, origin: &SourceUri) -> Result<Self, UiDocError> {
        let mut entries = self.entries.clone();
        for (key, value) in &other.entries {
            if entries.contains_key(key) {
                return Err(UiDocError::DuplicateTextKey {
                    origin: origin.clone(),
                    key: key.clone(),
                });
            }
            entries.insert(key.clone(), value.clone());
        }
        Ok(Self {
            id: self.id.clone(),
            schema: self.schema.clone(),
            version: self.version,
            entries,
        })
    }
}

/// Parses and validates a complete text catalog document.
///
/// # Errors
/// Returns [`UiDocError`] when the envelope or body is invalid.
pub fn parse_text(text: &str, origin: &SourceUri) -> Result<TextDoc, UiDocError> {
    let envelope = envelope::probe(text, origin)?;
    if envelope.kind != DocKind::Text {
        return Err(UiDocError::WrongDocKind {
            origin: origin.clone(),
            expected: DocKind::Text.name(),
            found: envelope.kind.name(),
        });
    }
    let document: TextDoc =
        ron_io::options()
            .from_str(text)
            .map_err(|source| UiDocError::Syntax {
                origin: origin.clone(),
                source: Box::new(source),
            })?;
    Ok(document)
}
