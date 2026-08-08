use bon::Builder;

use crate::{ids::InternId, mount::Control, size::SizeSpec, skin::SkinDoc};

/// A row of mutually exclusive segments, one of them picked.
#[derive(Builder)]
pub(crate) struct Segmented<'a> {
    pub(crate) items: &'a [InternId],
}

impl Control for Segmented<'_> {
    fn size(&self, skin: &SkinDoc) -> SizeSpec {
        skin.segmented.size
    }
}

#[cfg(feature = "render")]
mod host {
    use num_traits::ToPrimitive;

    use super::Segmented;
    use crate::{
        atoms::design::segmented::{Segmented as Face, SegmentedData},
        render::{
            ReadValue, Skin,
            controls::{Draws, Grip, Reading},
        },
    };

    impl Draws for Segmented<'_> {
        type Painter = Face;

        fn painter(&self, skin: &Skin) -> Face {
            Face::new(skin)
        }

        /// The picked cell is the reading rounded to a whole cell; a reading
        /// past the end of the row picks nothing rather than the last one.
        fn data(&self, read: Reading<'_>) -> Option<SegmentedData> {
            let Some(ReadValue::Scalar(value)) = read.value else {
                return None;
            };
            Some(SegmentedData {
                active: value
                    .round()
                    .to_usize()
                    .filter(|index| *index < self.items.len()),
                items: self
                    .items
                    .iter()
                    .map(|item| read.ui.resolve(*item).to_owned())
                    .collect(),
            })
        }

        fn grip(&self, _skin: &Skin, data: &SegmentedData) -> Grip {
            Grip::Index {
                count: data.items.len(),
            }
        }
    }
}
