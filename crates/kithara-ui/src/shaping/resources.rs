use std::fmt;

use kithara_platform::sync::Arc;
use parley::{
    FontData,
    fontique::{Blob, Collection, CollectionOptions, FallbackKey, Script},
};
#[cfg(feature = "render")]
use skrifa::{FontRef, outline::OutlineGlyphCollection, raw::ReadError};
use thiserror::Error;

use super::{FontId, FontPolicy, GlyphFace};

/// Failure to construct the embedded text resources.
#[derive(Clone, Debug, Error, PartialEq)]
#[non_exhaustive]
pub enum TextError {
    #[cfg(feature = "render")]
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
}

/// Scripts the Display family does not cover, answered by an embedded face.
///
/// A fallback key carries a script and a locale rather than a family, so this
/// is a collection-wide safety net: Inter and `JetBrains` Mono carry these
/// scripts themselves and never reach it, and only Space Grotesk does.
const DISPLAY_SCRIPT_COVERAGE: [Script; 2] = [Script(*b"Cyrl"), Script(*b"Grek")];

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
pub(crate) struct TextResources {
    collection: Collection,
    fonts: [FontId; 10],
    #[field(get, vis = "pub(super)", copy)]
    faces: FaceBlobs,
    policy: FontPolicy,
    #[cfg(feature = "render")]
    outlines: Vec<OutlineGlyphCollection<'static>>,
}

impl TextResources {
    pub(crate) fn new(policy: FontPolicy) -> Result<Self, TextError> {
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
        Ok(Self {
            collection,
            fonts: FontId::ALL,
            faces: FaceBlobs(blobs),
            policy,
            #[cfg(feature = "render")]
            outlines: FontId::ALL
                .into_iter()
                .map(outline_collection)
                .collect::<Result<_, _>>()?,
        })
    }

    pub(super) fn collection(&self) -> Collection {
        self.collection.clone()
    }

    #[cfg(feature = "render")]
    pub(crate) fn outlines(&self, font: FontId) -> &OutlineGlyphCollection<'static> {
        &self.outlines[font.index()]
    }
}

impl fmt::Debug for TextResources {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TextResources")
            .field("fonts", &self.fonts)
            .field("policy", &self.policy)
            .finish_non_exhaustive()
    }
}

impl PartialEq for TextResources {
    fn eq(&self, other: &Self) -> bool {
        self.fonts == other.fonts && self.policy == other.policy
    }
}

fn register_fallbacks(collection: &mut Collection) -> Result<(), TextError> {
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

#[cfg(feature = "render")]
fn outline_collection(font: FontId) -> Result<OutlineGlyphCollection<'static>, TextError> {
    let font_ref =
        FontRef::new(font.bytes()).map_err(|source| TextError::InvalidFont { font, source })?;
    Ok(OutlineGlyphCollection::new(&font_ref))
}
