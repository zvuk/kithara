use bon::Builder;

use crate::{expand::Binding, mount::Control, size::SizeSpec, skin::SkinDoc};

/// The library tree, with its own search field.
#[derive(Builder)]
pub(crate) struct Tree<'a> {
    pub(crate) query: Option<&'a Binding>,
}

impl Control for Tree<'_> {
    fn size(&self, skin: &SkinDoc) -> SizeSpec {
        skin.tree.size
    }
}
