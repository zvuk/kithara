use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::ron_io;
use crate::{
    envelope::{self, DocKind},
    error::UiDocError,
    ids::{DocId, ScreenRole, SourceUri},
    source::SourceResolver,
};

/// The ui contract this build offers a package.
///
/// It counts the vocabulary a package is written against - the roles asked
/// for, the endpoints answered, the extension kinds drawable - and not the
/// shape of any one document, which each document states for itself.
pub const UI_CONTRACT: u32 = 1;

/// What a package says about itself before any of its documents are read.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct PackageDoc {
    /// The file behind each role the package answers for.
    pub screens: BTreeMap<ScreenRole, String>,
    pub id: DocId,
    #[serde(default)]
    pub skin: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
    pub schema: String,
    /// Whether what this package does not hold is read from the one below it.
    #[serde(default)]
    pub inherits: bool,
    /// The ui contract the package was written against.
    pub contract: u32,
    pub version: u32,
}

impl PackageDoc {
    /// The file this package puts behind `role`, once that file agrees it is
    /// that screen.
    ///
    /// The manifest says which file stands for a role and the document says
    /// which screen it is; a package whose two answers disagree has a typo in
    /// one of them, and reading only the manifest would compile the wrong
    /// screen without a word. Only the envelope is parsed here, and the
    /// resolver has already read the text the compile will parse in full.
    ///
    /// # Errors
    /// Returns [`UiDocError`] when the package answers for no such screen, when
    /// the file behind it cannot be read, or when that file names another
    /// screen.
    pub fn screen(
        &self,
        resolver: &dyn SourceResolver,
        role: &ScreenRole,
    ) -> Result<String, UiDocError> {
        let file = self
            .screens
            .get(role)
            .ok_or_else(|| UiDocError::MissingRole {
                package: self.id.0.clone(),
                role: role.0.clone(),
            })?;
        let loaded = resolver.load(None, file)?;
        let envelope = envelope::probe(&loaded.text, &loaded.uri)?;
        if envelope.id.0 == role.0 {
            return Ok(file.clone());
        }
        Err(UiDocError::RoleMismatch {
            found: envelope.id.0,
            origin: loaded.uri,
            role: role.0.clone(),
        })
    }
}

/// Reads the manifest at `rel` and checks it before anything else is parsed.
///
/// The contract check comes first: a package written for another build is
/// refused here, while its documents are still unread, so the message names the
/// mismatch rather than whatever the first stale document happened to trip on.
///
/// # Errors
/// Returns [`UiDocError`] when the manifest is unavailable, malformed, written
/// against another contract, or declares nothing to answer with.
pub fn load_package(resolver: &dyn SourceResolver, rel: &str) -> Result<PackageDoc, UiDocError> {
    let loaded = resolver.load(None, rel)?;
    let doc = parse_package(&loaded.text, &loaded.uri)?;
    if doc.contract != UI_CONTRACT {
        return Err(UiDocError::ContractMismatch {
            needs: doc.contract,
            offers: UI_CONTRACT,
            origin: loaded.uri,
        });
    }
    if doc.screens.is_empty() {
        return Err(UiDocError::EmptyPackage { origin: loaded.uri });
    }
    for (role, file) in &doc.screens {
        if file.is_empty() {
            return Err(UiDocError::RoleWithoutFile {
                origin: loaded.uri,
                role: role.0.clone(),
            });
        }
    }
    Ok(doc)
}

/// Parses a package manifest.
///
/// # Errors
/// Returns [`UiDocError`] when the RON, schema, or version is invalid.
pub fn parse_package(text: &str, origin: &SourceUri) -> Result<PackageDoc, UiDocError> {
    let envelope = envelope::probe(text, origin)?;
    if envelope.kind != DocKind::Package {
        return Err(UiDocError::WrongDocKind {
            origin: origin.clone(),
            expected: DocKind::Package.name(),
            found: envelope.kind.name(),
        });
    }
    ron_io::options()
        .from_str(text)
        .map_err(|source| UiDocError::Syntax {
            origin: origin.clone(),
            source: Box::new(source),
        })
}
