use bon::Builder;

use crate::{ids::InternId, module::Tone, mount::Control, size::SizeSpec, skin::SkinDoc};

/// A toned dot beside a word.
#[derive(Builder)]
pub(crate) struct StatusDot {
    pub(crate) label: InternId,
    pub(crate) tone: Tone,
}

impl Control for StatusDot {
    fn size(&self, skin: &SkinDoc) -> SizeSpec {
        skin.status_dot.size
    }
}

#[cfg(feature = "render")]
mod host {
    use super::StatusDot;
    use crate::{
        atoms::design::status_dot::StatusDot as Face,
        render::{
            Skin,
            controls::{Draws, Reading},
        },
    };

    impl Draws for StatusDot {
        type Painter = Face;

        fn painter(&self, skin: &Skin) -> Face {
            Face::new(self.tone, skin)
        }

        fn data(&self, read: Reading<'_>) -> Option<String> {
            Some(read.ui.resolve(self.label).to_owned())
        }
    }
}
