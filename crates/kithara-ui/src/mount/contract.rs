use crate::{size::SizeSpec, skin::SkinDoc};

/// One built-in control, described in the one file that owns it.
///
/// This half of the contract is what the document alone settles, so it is also
/// the half that still compiles when no renderer is compiled in. Each host adds
/// its own half as a separate trait, implemented for the same types.
pub(crate) trait Control {
    /// The size the skin gives this control, before any override the document
    /// declares on the node itself.
    fn size(&self, skin: &SkinDoc) -> SizeSpec;

    /// Whether that size is what a parent composes with. A control that says
    /// no takes its box from whatever holds it.
    fn composes_size(&self) -> bool {
        true
    }
}
