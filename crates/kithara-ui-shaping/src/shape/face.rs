use std::borrow::Cow;

use kithara_platform::sync::Arc;
use parley::{FontData, fontique::Blob};

use crate::FontId;

/// A font face resolved for one shaped glyph segment.
#[derive(Clone, Debug, PartialEq)]
pub enum GlyphFace {
    /// A face from the embedded catalog.
    Embedded(FontId),
    /// A face resolved from the machine's system collection.
    System(FontData),
}

impl<'a> From<&'a GlyphFace> for Cow<'a, FontData> {
    fn from(face: &'a GlyphFace) -> Self {
        match face {
            GlyphFace::Embedded(font) => {
                Self::Owned(FontData::new(Blob::new(Arc::new(font.bytes())), 0))
            }
            GlyphFace::System(data) => Self::Borrowed(data),
        }
    }
}
