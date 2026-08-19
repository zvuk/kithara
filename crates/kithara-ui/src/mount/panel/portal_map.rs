use crate::{mount::Control, size::SizeSpec, skin::SkinDoc};

/// A tempo axis carrying one arc from the master tempo to each portal target.
pub(crate) struct PortalMap;

impl Control for PortalMap {
    fn size(&self, skin: &SkinDoc) -> SizeSpec {
        skin.portal_map.size
    }
}

#[cfg(feature = "render")]
mod host {
    use super::PortalMap;
    use crate::{
        atoms::pivot::map::{PortalMap as Face, PortalMapData},
        render::{
            ReadValue, Skin,
            controls::{Draws, Reading},
        },
    };

    impl Draws for PortalMap {
        type Painter = Face;

        fn painter(&self, skin: &Skin) -> Face {
            Face::new(skin)
        }

        /// A host that reports no map draws nothing: an axis with no tempo on
        /// it would claim a range the host never named.
        fn data(&self, read: Reading<'_>) -> Option<PortalMapData> {
            match read.value {
                Some(ReadValue::PortalMap(view)) => Some((*view).into()),
                _ => None,
            }
        }
    }
}
