use std::collections::BTreeMap;

use kithara_platform::sync::Arc;

use super::sprite::Sheet;
use crate::{error::UiDocError, skin::PictureDoc, source::SourceResolver};

/// Every picture one skin carries, cut into frames and kept by the name a
/// document asks for it by.
///
/// Cutting happens once, while the skin resolves, because a frame is its own
/// picture with its own identity: a rasteriser uploads each one once and every
/// later frame of the animation is a lookup.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub struct Pictures {
    sheets: BTreeMap<String, Arc<Sheet>>,
}

impl Pictures {
    /// Reads every picture the skin names and cuts it on the grid it declares.
    ///
    /// A picture the skin names and the resolver cannot answer is an error
    /// rather than an empty slot: the skin declared it, so a build without it
    /// is a broken skin, not a skin with one drawing fewer.
    ///
    /// # Errors
    /// Returns [`UiDocError`] when a picture is missing, escapes the root, or
    /// does not cut into the frames the skin declares.
    pub(crate) fn load(
        document: &PictureDoc,
        resolver: &dyn SourceResolver,
    ) -> Result<Self, UiDocError> {
        let mut sheets = BTreeMap::new();
        for (name, declared) in &document.sheets {
            let loaded = resolver.bytes(None, &declared.source)?;
            let cut = Sheet::cut(name, &loaded.bytes, declared.columns, declared.rows).map_err(
                |source| UiDocError::Picture {
                    name: name.clone(),
                    origin: loaded.uri,
                    source: Box::new(source),
                },
            )?;
            sheets.insert(name.clone(), Arc::new(cut));
        }
        Ok(Self { sheets })
    }

    /// The picture one name means, or nothing when this skin carries none by
    /// that name.
    #[must_use]
    pub fn sheet(&self, name: &str) -> Option<&Arc<Sheet>> {
        self.sheets.get(name)
    }
}
