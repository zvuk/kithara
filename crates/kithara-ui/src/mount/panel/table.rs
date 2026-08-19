use bon::Builder;

use crate::{expand::Binding, module::TableColumn, mount::Control, size::SizeSpec, skin::SkinDoc};

/// A table whose columns and row values are supplied by the document and host.
#[derive(Builder)]
pub(crate) struct Table<'a> {
    pub(crate) columns: &'a [TableColumn],
    pub(crate) columns_state: Option<&'a Binding>,
}

impl Control for Table<'_> {
    fn size(&self, skin: &SkinDoc) -> SizeSpec {
        skin.table.size
    }
}
