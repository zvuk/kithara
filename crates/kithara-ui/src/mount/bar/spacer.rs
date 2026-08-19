use crate::{mount::Control, size::SizeSpec, skin::SkinDoc};

/// Empty room that pushes its neighbours apart.
pub(crate) struct Spacer;

impl Control for Spacer {
    fn size(&self, skin: &SkinDoc) -> SizeSpec {
        skin.global_bar.spacer_size
    }
}

#[cfg(feature = "render")]
mod host {
    use super::Spacer;
    use crate::{
        atoms::bar::spacer::Spacer as Face,
        render::{
            Skin,
            controls::{Draws, Reading},
        },
    };

    impl Draws for Spacer {
        type Painter = Face;

        fn painter(&self, skin: &Skin) -> Face {
            Face::new(skin)
        }

        fn data(&self, _read: Reading<'_>) -> Option<()> {
            Some(())
        }
    }
}
