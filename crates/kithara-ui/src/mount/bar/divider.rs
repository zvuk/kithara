use crate::{
    mount::Control,
    size::{Dim, SizeSpec},
    skin::SkinDoc,
};

/// A hairline separating two runs of a bar.
pub(crate) struct Divider;

impl Control for Divider {
    fn size(&self, skin: &SkinDoc) -> SizeSpec {
        SizeSpec::new(Dim::Fixed(skin.divider.width), Dim::Fill)
    }
}

#[cfg(feature = "render")]
mod host {
    use super::Divider;
    use crate::{
        atoms::bar::divider::Divider as Face,
        render::{
            Skin,
            controls::{Draws, Reading},
        },
    };

    impl Draws for Divider {
        type Painter = Face;

        fn painter(&self, skin: &Skin) -> Face {
            Face::new(skin)
        }

        fn data(&self, _read: Reading<'_>) -> Option<()> {
            Some(())
        }
    }
}
