use crate::{mount::Control, size::SizeSpec, skin::SkinDoc};

/// An interval with a handle at each end, each writing its own endpoint.
pub(crate) struct Range;

impl Control for Range {
    fn size(&self, skin: &SkinDoc) -> SizeSpec {
        skin.range.size
    }
}

#[cfg(feature = "render")]
mod host {
    use super::Range;
    use crate::{
        atoms::pivot::range::Range as Face,
        interact::CursorShape,
        render::{
            ReadValue, ScalarRange, Skin,
            controls::{Draws, Grip, Reading, Span},
        },
    };

    impl Draws for Range {
        type Painter = Face;

        fn painter(&self, skin: &Skin) -> Face {
            Face::new(skin)
        }

        /// An unbound range is an empty box rather than a full rail: the host
        /// has not said where either handle sits.
        fn data(&self, read: Reading<'_>) -> Option<ScalarRange> {
            match read.value {
                Some(ReadValue::Range(value)) => Some(*value),
                _ => None,
            }
        }

        fn grip(&self, _skin: &Skin, data: &ScalarRange) -> Grip {
            Grip::Span(
                Span::builder()
                    .cursor(CursorShape::ResizeH)
                    .value(*data)
                    .build(),
            )
        }
    }
}
