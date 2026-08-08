use crate::{mount::Control, size::SizeSpec, skin::SkinDoc};

/// The global bar's preset picker.
pub(crate) struct Preset;

impl Control for Preset {
    fn size(&self, skin: &SkinDoc) -> SizeSpec {
        skin.global_bar.preset_size
    }
}
