use bon::Builder;

use crate::{
    ids::InternId,
    mount::Control,
    size::SizeSpec,
    skin::{ColorRole, SkinDoc},
};

/// One palette colour, shown with its name.
#[derive(Builder)]
pub(crate) struct Swatch {
    pub(crate) label: InternId,
    pub(crate) role: ColorRole,
}

impl Control for Swatch {
    fn size(&self, skin: &SkinDoc) -> SizeSpec {
        skin.swatch.size
    }
}

#[cfg(feature = "render")]
mod host {
    use super::Swatch;
    use crate::{
        atoms::design::swatch::Swatch as Face,
        render::{
            Skin,
            controls::{Draws, Reading},
        },
    };

    impl Draws for Swatch {
        type Painter = Face;

        fn painter(&self, skin: &Skin) -> Face {
            Face::new(self.role, skin)
        }

        fn data(&self, read: Reading<'_>) -> Option<String> {
            Some(read.ctx.ui.resolve(self.label).to_owned())
        }
    }
}
