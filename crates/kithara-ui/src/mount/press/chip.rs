use bon::Builder;

use crate::{ids::InternId, module::ChipStyle, mount::Control, size::SizeSpec, skin::SkinDoc};

/// A small labelled toggle that reads as a tag.
#[derive(Builder)]
pub(crate) struct Chip {
    pub(crate) label: InternId,
    pub(crate) style: ChipStyle,
}

impl Control for Chip {
    fn size(&self, skin: &SkinDoc) -> SizeSpec {
        skin.chip.size
    }
}

#[cfg(feature = "render")]
mod host {
    use super::Chip;
    use crate::{
        atoms::{chip::Chip as Face, painter::Labelled},
        render::{
            ReadValue, Skin,
            controls::{Draws, Grip, Reading},
        },
    };

    impl Draws for Chip {
        type Painter = Face;

        fn painter(&self, skin: &Skin) -> Face {
            Face::new(self.style, skin)
        }

        /// A chip carries a word the document wrote, so it shows it whether or
        /// not an endpoint has said which way it is set — unlike a switch,
        /// which is nothing but its state.
        fn data(&self, read: Reading<'_>) -> Option<Labelled> {
            Some(Labelled {
                active: matches!(read.value, Some(ReadValue::Bool(true))),
                label: read.ui.resolve(self.label).to_owned(),
            })
        }

        fn grip(&self, _skin: &Skin, _data: &Labelled) -> Grip {
            Grip::Press
        }
    }
}
