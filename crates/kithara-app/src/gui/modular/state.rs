use kithara_ui::{builtin, compile::CompiledUi};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum ViewMode {
    #[default]
    Compact,
    Studio,
    Modular,
}

pub(crate) struct ModularView {
    pub(crate) preset: String,
    pub(crate) compiled: Option<CompiledUi>,
    pub(crate) error: Option<String>,
}

impl Default for ModularView {
    fn default() -> Self {
        Self {
            preset: builtin::MICRO_PRESET.to_owned(),
            compiled: None,
            error: None,
        }
    }
}
