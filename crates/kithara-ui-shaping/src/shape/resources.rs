use std::collections::BTreeMap;

use kithara_platform::sync::Arc;
use parley::{
    FontData,
    fontique::{Blob, Collection, CollectionOptions, FallbackKey, FontInfoOverride, Script},
};
use skrifa::{FontRef, outline::OutlineGlyphCollection, raw::ReadError};
use thiserror::Error;

use crate::{FontFamily, FontId, FontPolicy, GlyphFace};

/// Failure to construct the embedded text resources.
#[derive(Clone, Debug, Error, PartialEq)]
#[non_exhaustive]
pub enum TextError {
    #[error("embedded font face {font:?} is invalid: {source}")]
    InvalidFont {
        font: FontId,
        #[source]
        source: ReadError,
    },
    #[error("embedded font face {font:?} could not be registered")]
    Registration { font: FontId },
    #[error("embedded fallback for script {script} could not be registered")]
    Fallback { script: Script },
    #[error("font family {family:?} declares no source files")]
    EmptyFamily { family: FontFamily },
    #[error("font source for {family:?} is invalid: {source}")]
    InvalidSource {
        family: FontFamily,
        #[source]
        source: ReadError,
    },
    #[error("font source for {family:?} could not be registered")]
    SourceRegistration { family: FontFamily },
}

/// Maps a registered Fontique blob back to the face it was registered for.
///
/// Registration is the one place a face and a blob meet, so identity is
/// recorded there. Matching a shaped run on the `&'static [u8]` address
/// instead looks equivalent and is not: `include_bytes!` is a promoted
/// constant, and nothing obliges the compiler to give two evaluations of it
/// the same address. It happened to hold in a single-crate build and not in a
/// workspace one.
#[derive(Clone, Copy)]
pub(super) struct FaceBlobs([u64; 10]);

impl FaceBlobs {
    pub(super) fn resolve(self, data: &FontData) -> GlyphFace {
        self.0
            .iter()
            .position(|id| *id == data.data.id())
            .and_then(|index| FontId::ALL.get(index))
            .copied()
            .map_or_else(|| GlyphFace::System(data.clone()), GlyphFace::Embedded)
    }
}

#[derive(Clone, fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
#[derive(derive_more::Debug)]
pub struct TextResources {
    #[debug(skip)]
    collection: Collection,
    fonts: [FontId; 10],
    #[field(get, vis = "pub(super)", copy)]
    #[debug(skip)]
    faces: FaceBlobs,
    policy: FontPolicy,
    #[debug(skip)]
    outlines: Vec<OutlineGlyphCollection<'static>>,
    #[field(get, vis = "pub(super)")]
    families: BTreeMap<&'static str, &'static str>,
    #[debug(skip)]
    sources: BTreeMap<FontFamily, Vec<Arc<[u8]>>>,
}

impl TextResources {
    /// Registers the embedded faces, and the system collection when `policy`
    /// asks for it.
    ///
    /// # Errors
    ///
    /// Returns [`TextError`] when an embedded face or fallback cannot be
    /// registered.
    pub fn new(policy: FontPolicy) -> Result<Self, TextError> {
        Self::with_fonts(policy, BTreeMap::new())
    }

    /// Registers skin-owned font files in place of the named style families.
    /// Each family may carry static faces or one variable font. The bytes stay
    /// with the collection and the shaped result; embedded defaults and Lucide
    /// remain independent of these private family aliases.
    ///
    /// # Errors
    /// Returns [`TextError`] for an empty family, invalid font, or failed
    /// registration.
    pub fn with_fonts(
        policy: FontPolicy,
        sources: BTreeMap<FontFamily, Vec<Arc<[u8]>>>,
    ) -> Result<Self, TextError> {
        let mut collection = Collection::new(CollectionOptions {
            shared: false,
            system_fonts: policy.system_fonts(),
        });
        let mut blobs = [0; 10];
        for (font, blob) in FontId::ALL.into_iter().zip(&mut blobs) {
            let data = Blob::new(Arc::new(font.bytes()));
            *blob = data.id();
            if collection.register_fonts(data, None).is_empty() {
                return Err(TextError::Registration { font });
            }
        }
        register_fallbacks(&mut collection)?;
        let mut families = BTreeMap::new();
        for (&family, files) in &sources {
            if files.is_empty() {
                return Err(TextError::EmptyFamily { family });
            }
            let alias = match family {
                FontFamily::Sans => "kithara-skin-sans",
                FontFamily::Mono => "kithara-skin-mono",
                FontFamily::Display => "kithara-skin-display",
            };
            for bytes in files {
                FontRef::new(bytes)
                    .map_err(|source| TextError::InvalidSource { family, source })?;
                let data = Blob::new(Arc::new(bytes.clone()));
                let info = FontInfoOverride {
                    family_name: Some(alias),
                    ..FontInfoOverride::default()
                };
                if collection.register_fonts(data, Some(info)).is_empty() {
                    return Err(TextError::SourceRegistration { family });
                }
            }
            families.insert(
                FontId::select(family, crate::FontWeight::Normal).family_name(),
                alias,
            );
        }
        Ok(Self {
            collection,
            policy,
            fonts: FontId::ALL,
            faces: FaceBlobs(blobs),
            outlines: FontId::ALL
                .into_iter()
                .map(outline_collection)
                .collect::<Result<_, _>>()?,
            families,
            sources,
        })
    }

    pub(crate) fn collection(&self) -> Collection {
        self.collection.clone()
    }

    /// Returns the outline collection of one embedded face.
    #[must_use]
    pub fn outlines(&self, font: FontId) -> &OutlineGlyphCollection<'static> {
        &self.outlines[font.index()]
    }
}

impl PartialEq for TextResources {
    fn eq(&self, other: &Self) -> bool {
        self.fonts == other.fonts && self.policy == other.policy && self.sources == other.sources
    }
}

fn register_fallbacks(collection: &mut Collection) -> Result<(), TextError> {
    /// Scripts the Display family does not cover, answered by an embedded face.
    ///
    /// A fallback key carries a script and a locale rather than a family, so this
    /// is a collection-wide safety net: Inter and `JetBrains` Mono carry these
    /// scripts themselves and never reach it, and only Space Grotesk does.
    const DISPLAY_SCRIPT_COVERAGE: [Script; 2] = [Script(*b"Cyrl"), Script(*b"Grek")];

    let font = FontId::InterRegular;
    let Some(family) = collection.family_id(font.family_name()) else {
        return Err(TextError::Registration { font });
    };
    for script in DISPLAY_SCRIPT_COVERAGE {
        if !collection.append_fallbacks(FallbackKey::new(script, None), [family].into_iter()) {
            return Err(TextError::Fallback { script });
        }
    }
    Ok(())
}
fn outline_collection(font: FontId) -> Result<OutlineGlyphCollection<'static>, TextError> {
    let font_ref =
        FontRef::new(font.bytes()).map_err(|source| TextError::InvalidFont { font, source })?;
    Ok(OutlineGlyphCollection::new(&font_ref))
}
