use std::collections::BTreeSet;

use kithara_ui::{builtin, compile::CompiledUi};

pub(crate) struct ModularView {
    pub(crate) hidden: BTreeSet<String>,
    pub(crate) compiled: Option<CompiledUi>,
    pub(crate) error: Option<String>,
    pub(crate) preset: String,
    pub(crate) settings: Option<SettingsDraft>,
}

impl Default for ModularView {
    fn default() -> Self {
        Self {
            hidden: BTreeSet::new(),
            compiled: None,
            error: None,
            preset: builtin::MICRO_PRESET.to_owned(),
            settings: None,
        }
    }
}

pub(crate) struct SettingsDraft {
    pub(crate) draft_hidden: BTreeSet<String>,
    pub(crate) draft_preset: BuiltinPreset,
    pub(crate) section: SettingsSection,
    micro: CompiledUi,
    player: CompiledUi,
}

impl SettingsDraft {
    pub(crate) fn new(
        preset: &str,
        hidden: BTreeSet<String>,
        micro: CompiledUi,
        player: CompiledUi,
    ) -> Option<Self> {
        Some(Self {
            draft_hidden: hidden,
            draft_preset: BuiltinPreset::try_from(preset).ok()?,
            section: SettingsSection::Layout,
            micro,
            player,
        })
    }

    pub(crate) const fn compiled(&self) -> &CompiledUi {
        match self.draft_preset {
            BuiltinPreset::Micro => &self.micro,
            BuiltinPreset::Player => &self.player,
        }
    }

    pub(crate) const fn micro(&self) -> &CompiledUi {
        &self.micro
    }

    pub(crate) const fn player(&self) -> &CompiledUi {
        &self.player
    }

    pub(crate) fn select_preset(&mut self, preset: &str) {
        let Ok(preset) = BuiltinPreset::try_from(preset) else {
            return;
        };
        if self.draft_preset != preset {
            self.draft_preset = preset;
            self.draft_hidden.clear();
        }
    }

    pub(crate) fn toggle_module(&mut self, instance: String) {
        if !self.draft_hidden.remove(&instance) {
            self.draft_hidden.insert(instance);
        }
    }

    pub(crate) fn reset(&mut self) {
        self.draft_hidden.clear();
    }
}

/// Settings draft resolved into the state the canvas should apply.
pub(crate) struct AppliedSettings {
    pub(crate) preset: &'static str,
    pub(crate) hidden: BTreeSet<String>,
    pub(crate) compiled: CompiledUi,
}

impl From<SettingsDraft> for AppliedSettings {
    fn from(draft: SettingsDraft) -> Self {
        let SettingsDraft {
            draft_hidden,
            draft_preset,
            micro,
            player,
            ..
        } = draft;
        let compiled = match draft_preset {
            BuiltinPreset::Micro => micro,
            BuiltinPreset::Player => player,
        };
        Self {
            preset: <&'static str>::from(draft_preset),
            hidden: draft_hidden,
            compiled,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BuiltinPreset {
    Micro,
    Player,
}

impl From<BuiltinPreset> for &'static str {
    fn from(preset: BuiltinPreset) -> Self {
        match preset {
            BuiltinPreset::Micro => builtin::MICRO_PRESET,
            BuiltinPreset::Player => builtin::PLAYER_PRESET,
        }
    }
}

impl TryFrom<&str> for BuiltinPreset {
    type Error = ();

    fn try_from(preset: &str) -> Result<Self, Self::Error> {
        match preset {
            builtin::MICRO_PRESET => Ok(Self::Micro),
            builtin::PLAYER_PRESET => Ok(Self::Player),
            _ => Err(()),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SettingsSection {
    Layout,
    Modules,
}
