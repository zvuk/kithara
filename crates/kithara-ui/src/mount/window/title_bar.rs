use bon::Builder;

use crate::{ids::InternId, mount::Control, size::SizeSpec, skin::SkinDoc};

/// The window's own caption strip, which also drags the window.
#[derive(Builder)]
pub(crate) struct TitleBar {
    pub(crate) label: InternId,
}

impl Control for TitleBar {
    fn size(&self, _skin: &SkinDoc) -> SizeSpec {
        SizeSpec::FILL
    }
}
