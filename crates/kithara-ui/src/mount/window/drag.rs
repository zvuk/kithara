use crate::{mount::Control, size::SizeSpec, skin::SkinDoc};

/// A region that moves the window rather than drawing anything.
pub(crate) struct Drag;

impl Control for Drag {
    fn size(&self, _skin: &SkinDoc) -> SizeSpec {
        SizeSpec::FILL
    }
}
