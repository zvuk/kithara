use crate::{mount::Control, size::SizeSpec, skin::SkinDoc};

/// The visualiser surface.
pub(crate) struct Vis;

impl Control for Vis {
    fn size(&self, skin: &SkinDoc) -> SizeSpec {
        skin.vis.size
    }
}
