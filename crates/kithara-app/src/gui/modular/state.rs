use std::collections::BTreeSet;

use kithara_ui::{builtin, compile::CompiledUi};

pub(crate) struct ModularView {
    pub(crate) hidden: BTreeSet<String>,
    pub(crate) compiled: Option<CompiledUi>,
    pub(crate) error: Option<String>,
    pub(crate) preset: String,
}

impl Default for ModularView {
    fn default() -> Self {
        Self {
            hidden: BTreeSet::new(),
            compiled: None,
            error: None,
            preset: builtin::MICRO_PRESET.to_owned(),
        }
    }
}
