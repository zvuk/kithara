use crate::{mount::Control, size::SizeSpec, skin::SkinDoc};

/// The wordmark at the head of the global bar.
pub(crate) struct Brand;

impl Control for Brand {
    fn size(&self, skin: &SkinDoc) -> SizeSpec {
        skin.global_bar.brand_size
    }
}

#[cfg(feature = "render")]
mod host {
    use super::Brand;
    use crate::{
        atoms::bar::brand::Brand as Face,
        render::{
            Skin,
            controls::{Draws, Reading},
        },
    };

    impl Draws for Brand {
        type Painter = Face;

        fn painter(&self, skin: &Skin) -> Face {
            Face::new(skin)
        }

        fn data(&self, _read: Reading<'_>) -> Option<()> {
            Some(())
        }
    }
}
