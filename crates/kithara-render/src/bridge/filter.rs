use std::fmt;

use kithara_config::ConfigOwner;
use kithara_effects::filter::{FilterEffect, FilterSettings, FilterSettingsChange};

use super::FilterBinding;

/// A prepared replacement or one live operation of the deck's post-FX filter.
pub enum DeckFilterChange {
    Install(Box<FilterEffect>),
    Clear,
    Configure(FilterSettingsChange),
    Reset,
}

impl fmt::Debug for DeckFilterChange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Install(filter) => f.debug_tuple("Install").field(filter.config()).finish(),
            Self::Clear => f.write_str("Clear"),
            Self::Configure(change) => f.debug_tuple("Configure").field(change).finish(),
            Self::Reset => f.write_str("Reset"),
        }
    }
}

/// Settings published by the executor, with their instance and stream identity.
#[derive(Clone, Copy, Debug, Default, PartialEq, fieldwork::Fieldwork)]
#[fieldwork(get)]
pub struct FilterSnapshot {
    #[field(get, copy)]
    settings: Option<FilterSettings>,
    #[field(get, copy)]
    binding: Option<FilterBinding>,
}

impl FilterSnapshot {
    pub(crate) fn publish(
        &mut self,
        settings: Option<FilterSettings>,
        binding: Option<FilterBinding>,
    ) {
        self.settings = settings;
        self.binding = binding;
    }
}
