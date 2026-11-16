use std::collections::BTreeMap;

use kithara_platform::sync::Arc;

use super::sprite::Sheet;
use crate::{error::UiDocError, lottie::Artwork, skin::PictureDoc, source::SourceResolver};

/// Every picture one skin carries, cut into frames and kept by the name a
/// document asks for it by.
///
/// Cutting happens once, while the skin resolves, because a frame is its own
/// picture with its own identity: a rasteriser uploads each one once and every
/// later frame of the animation is a lookup.
#[derive(Clone, Default)]
#[non_exhaustive]
pub struct Pictures {
    sheets: BTreeMap<String, Arc<Sheet>>,
    artworks: BTreeMap<String, Arc<Artwork>>,
}

impl std::fmt::Debug for Pictures {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Pictures")
            .field("sheets", &self.sheets)
            .field("artworks", &self.artworks.keys())
            .finish()
    }
}
impl PartialEq for Pictures {
    fn eq(&self, other: &Self) -> bool {
        self.sheets == other.sheets
            && self.artworks.len() == other.artworks.len()
            && self.artworks.iter().all(|(name, art)| {
                other
                    .artworks
                    .get(name)
                    .is_some_and(|other| Arc::ptr_eq(art, other))
            })
    }
}
impl Eq for Pictures {}

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
        declared_artworks: &BTreeMap<String, String>,
        origin: &crate::ids::SourceUri,
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
        let mut artworks = BTreeMap::new();
        if declared_artworks.len() > 64 {
            return Err(UiDocError::Artwork {
                origin: origin.clone(),
                name: String::new(),
                reason: "more than 64 artworks".into(),
            });
        }
        for (name, source) in declared_artworks {
            let loaded = resolver.bytes(None, source).map_err(|error| match error {
                UiDocError::NotFound { rel, .. } => UiDocError::NotFound {
                    origin: origin.clone(),
                    rel,
                },
                other => other,
            })?;
            let invalid = |reason: String| UiDocError::Artwork {
                origin: loaded.uri.clone(),
                name: name.clone(),
                reason,
            };
            let artwork =
                Artwork::parse(&loaded.bytes).map_err(|error| invalid(error.to_string()))?;
            artworks.insert(name.clone(), Arc::new(artwork));
        }
        Ok(Self { sheets, artworks })
    }

    /// The picture one name means, or nothing when this skin carries none by
    /// that name.
    #[must_use]
    pub fn sheet(&self, name: &str) -> Option<&Arc<Sheet>> {
        self.sheets.get(name)
    }
    pub(crate) fn artwork(&self, name: &str) -> Option<&Arc<Artwork>> {
        self.artworks.get(name)
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{ids::SourceUri, source::MemResolver};

    const PULSE: &[u8] = include_bytes!("../../../../kithara-ui-lottie/assets/lottie/pulse.json");

    #[kithara::test]
    fn a_skin_owns_its_artworks_and_clones_share_the_parsed_resource() {
        let mut resolver = MemResolver::default();
        resolver.insert_bytes("party.json", PULSE);
        let declared = BTreeMap::from([("party".into(), "party.json".into())]);
        let origin = SourceUri("party.kskin.ron".into());
        let first = Pictures::load(&PictureDoc::default(), &declared, &origin, &resolver).unwrap();
        let clone = first.clone();
        assert!(Arc::ptr_eq(
            first.artwork("party").unwrap(),
            clone.artwork("party").unwrap()
        ));
        let second = Pictures::load(&PictureDoc::default(), &declared, &origin, &resolver).unwrap();
        assert!(!Arc::ptr_eq(
            first.artwork("party").unwrap(),
            second.artwork("party").unwrap()
        ));
        assert!(first.artwork("undeclared").is_none());
    }

    #[kithara::test]
    fn declared_malformed_and_oversized_artwork_is_a_load_error() {
        let mut resolver = MemResolver::default();
        let declared = BTreeMap::from([("party".into(), "party.json".into())]);
        let origin = SourceUri("party.kskin.ron".into());
        resolver.insert_bytes("party.json", b"not-json");
        assert!(matches!(
            Pictures::load(&PictureDoc::default(), &declared, &origin, &resolver),
            Err(UiDocError::Artwork { .. })
        ));
        resolver.insert_bytes("party.json", &vec![b' '; 128 * 1024 + 1]);
        assert!(matches!(
            Pictures::load(&PictureDoc::default(), &declared, &origin, &resolver),
            Err(UiDocError::Artwork { .. })
        ));
    }
}
