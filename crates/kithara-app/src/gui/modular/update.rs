use iced::{Task, window};
use kithara_ui::{
    builtin,
    compile::{CompiledUi, compile},
    render::UiEvent,
    source::UiConfig,
};
use tracing::error;

use super::{
    ModularView, dispatch, endpoints, settings,
    state::{AppliedSettings, SettingsDraft, SettingsSection},
};
use crate::gui::{app::Kithara, frontend::window_settings, message::Message};

pub(crate) fn update(state: &mut Kithara, message: UiEvent) -> Task<Message> {
    match message {
        UiEvent::SelectPreset(preset) => select_preset(state, &preset),
        UiEvent::LibraryQuery(query) => {
            state.library_query = query;
            Task::none()
        }
        UiEvent::ToggleModule(instance) => {
            if !state.modular.hidden.remove(&instance) {
                state.modular.hidden.insert(instance);
            }
            Task::none()
        }
        UiEvent::OpenSettings => open_settings(state),
        UiEvent::CloseSettings => cancel_settings(state),
        UiEvent::SettingsShowLayout => {
            show_settings_section(&mut state.modular, SettingsSection::Layout);
            Task::none()
        }
        UiEvent::SettingsShowModules => {
            show_settings_section(&mut state.modular, SettingsSection::Modules);
            Task::none()
        }
        UiEvent::SettingsSelectPreset(preset) => {
            select_settings_preset(&mut state.modular, &preset);
            Task::none()
        }
        UiEvent::SettingsToggleModule(instance) => {
            toggle_settings_module(&mut state.modular, instance);
            Task::none()
        }
        UiEvent::SettingsReset => {
            reset_settings_draft(&mut state.modular);
            Task::none()
        }
        UiEvent::SettingsDone => done_settings(state),
        UiEvent::Control { path, action } => {
            dispatch::apply(state, &path, &action);
            Task::none()
        }
        _ => Task::none(),
    }
}

pub(crate) fn initial_view() -> ModularView {
    let preset = builtin::PLAYER_PRESET;
    let mut modular = ModularView::default();
    match compile_preset(preset) {
        Ok(compiled) => commit_preset(&mut modular, preset, compiled),
        Err(error) => {
            error!(error = %error, preset, "initial modular preset compile failed");
            modular.preset = preset.to_owned();
            modular.error = Some(error);
        }
    }
    modular
}

fn select_preset(state: &mut Kithara, preset: &str) -> Task<Message> {
    match compile_preset(preset) {
        Ok(compiled) => {
            commit_preset(&mut state.modular, preset, compiled);
            replace_main_window(state)
        }
        Err(error) => {
            error!(error = %error, preset, "modular preset compile failed");
            state.modular.error = Some(error);
            Task::none()
        }
    }
}

fn compile_preset(preset: &str) -> Result<CompiledUi, String> {
    compile(
        preset,
        &builtin::resolver(),
        &endpoints::registry(),
        &UiConfig::default(),
    )
    .map_err(|error| error.to_string())
}

fn commit_preset(modular: &mut ModularView, preset: &str, compiled: CompiledUi) {
    if modular.preset != preset {
        modular.hidden.clear();
    }
    modular.preset = preset.to_owned();
    modular.compiled = Some(compiled);
    modular.error = None;
}

fn begin_settings(modular: &mut ModularView, micro: CompiledUi, player: CompiledUi) -> bool {
    let Some(draft) = SettingsDraft::new(&modular.preset, modular.hidden.clone(), micro, player)
    else {
        return false;
    };
    modular.settings = Some(draft);
    true
}

fn select_settings_preset(modular: &mut ModularView, preset: &str) {
    if let Some(draft) = modular.settings.as_mut() {
        draft.select_preset(preset);
    }
}

fn toggle_settings_module(modular: &mut ModularView, instance: String) {
    if let Some(draft) = modular.settings.as_mut() {
        draft.toggle_module(instance);
    }
}

fn show_settings_section(modular: &mut ModularView, section: SettingsSection) {
    if let Some(draft) = modular.settings.as_mut() {
        draft.section = section;
    }
}

fn cancel_settings_draft(modular: &mut ModularView) {
    modular.settings = None;
}

fn reset_settings_draft(modular: &mut ModularView) {
    if let Some(draft) = modular.settings.as_mut() {
        draft.reset();
    }
}

fn commit_settings_draft(modular: &mut ModularView) -> Option<bool> {
    let draft = modular.settings.take()?;
    let AppliedSettings {
        preset,
        hidden,
        compiled,
    } = draft.into();
    let preset_changed = modular.preset != preset;
    commit_preset(modular, preset, compiled);
    modular.hidden = hidden;
    Some(preset_changed)
}

fn compile_settings_presets() -> Result<(CompiledUi, CompiledUi), String> {
    let micro = compile_preset(builtin::MICRO_PRESET)?;
    let player = compile_preset(builtin::PLAYER_PRESET)?;
    Ok((micro, player))
}

fn done_settings(state: &mut Kithara) -> Task<Message> {
    let Some(preset_changed) = commit_settings_draft(&mut state.modular) else {
        return Task::none();
    };
    let close = close_settings_window(state);
    if preset_changed {
        close.chain(replace_main_window(state))
    } else {
        close
    }
}

fn replace_main_window(state: &mut Kithara) -> Task<Message> {
    let old = state.window_id;
    let (new_id, open) = window::open(window_settings(
        state.modular.compiled.as_ref(),
        &state.window_sizing,
    ));
    state.window_id = Some(new_id);
    let close_old = old.map_or_else(Task::none, window::close);
    open.discard().chain(close_old)
}

fn open_settings(state: &mut Kithara) -> Task<Message> {
    if state.settings_window_id.is_some() {
        return Task::none();
    }
    let (micro, player) = match compile_settings_presets() {
        Ok(presets) => presets,
        Err(error) => {
            error!(error = %error, "settings preset compile failed");
            return Task::none();
        }
    };
    if !begin_settings(&mut state.modular, micro, player) {
        error!(
            preset = state.modular.preset,
            "settings preset is not builtin"
        );
        return Task::none();
    }
    let (id, open) = window::open(settings::window_settings());
    state.settings_window_id = Some(id);
    open.discard()
}

fn cancel_settings(state: &mut Kithara) -> Task<Message> {
    cancel_settings_draft(&mut state.modular);
    close_settings_window(state)
}

fn close_settings_window(state: &mut Kithara) -> Task<Message> {
    state
        .settings_window_id
        .take()
        .map_or_else(Task::none, window::close)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use kithara_test_utils::kithara;
    use kithara_ui::builtin;

    use super::{
        begin_settings, cancel_settings_draft, commit_preset, commit_settings_draft,
        compile_preset, reset_settings_draft, select_settings_preset, toggle_settings_module,
    };
    use crate::gui::modular::{ModularView, state::BuiltinPreset};

    #[kithara::test]
    fn preset_change_clears_hidden_modules() {
        let mut modular = ModularView::default();
        modular.hidden.insert("deck-a".to_owned());
        let compiled = compile_preset(builtin::PLAYER_PRESET)
            .unwrap_or_else(|error| panic!("player preset must compile: {error}"));

        commit_preset(&mut modular, builtin::PLAYER_PRESET, compiled);

        assert!(modular.hidden.is_empty());
        assert_eq!(modular.preset, builtin::PLAYER_PRESET);
    }

    #[kithara::test]
    fn settings_cancel_keeps_applied_state() {
        let mut modular = ModularView::default();
        let micro = compile_preset(builtin::MICRO_PRESET)
            .unwrap_or_else(|error| panic!("micro preset must compile: {error}"));
        let player = compile_preset(builtin::PLAYER_PRESET)
            .unwrap_or_else(|error| panic!("player preset must compile: {error}"));
        modular.hidden.insert("deck-a".to_owned());

        assert!(begin_settings(&mut modular, micro, player));
        select_settings_preset(&mut modular, builtin::PLAYER_PRESET);
        toggle_settings_module(&mut modular, "library".to_owned());
        cancel_settings_draft(&mut modular);

        assert_eq!(modular.preset, builtin::MICRO_PRESET);
        assert_eq!(modular.hidden, BTreeSet::from(["deck-a".to_owned()]));
        assert!(modular.settings.is_none());
    }

    #[kithara::test]
    fn settings_done_applies_draft() {
        let mut modular = ModularView::default();
        let micro = compile_preset(builtin::MICRO_PRESET)
            .unwrap_or_else(|error| panic!("micro preset must compile: {error}"));
        let player = compile_preset(builtin::PLAYER_PRESET)
            .unwrap_or_else(|error| panic!("player preset must compile: {error}"));

        assert!(begin_settings(&mut modular, micro, player));
        select_settings_preset(&mut modular, builtin::PLAYER_PRESET);
        toggle_settings_module(&mut modular, "library".to_owned());

        assert_eq!(commit_settings_draft(&mut modular), Some(true));
        assert_eq!(modular.preset, builtin::PLAYER_PRESET);
        assert_eq!(modular.hidden, BTreeSet::from(["library".to_owned()]));
        assert!(matches!(
            modular.compiled.as_ref().map(|ui| &ui.root),
            Some(kithara_ui::compile::CompiledNode::Split { .. })
        ));
        assert!(modular.settings.is_none());
    }

    #[kithara::test]
    fn settings_reset_restores_selected_preset_defaults() {
        let mut modular = ModularView::default();
        let micro = compile_preset(builtin::MICRO_PRESET)
            .unwrap_or_else(|error| panic!("micro preset must compile: {error}"));
        let player = compile_preset(builtin::PLAYER_PRESET)
            .unwrap_or_else(|error| panic!("player preset must compile: {error}"));
        modular.hidden.insert("deck-a".to_owned());

        assert!(begin_settings(&mut modular, micro, player));
        select_settings_preset(&mut modular, builtin::PLAYER_PRESET);
        toggle_settings_module(&mut modular, "library".to_owned());
        reset_settings_draft(&mut modular);

        let draft = modular
            .settings
            .as_ref()
            .unwrap_or_else(|| panic!("settings draft must remain open"));
        assert_eq!(draft.draft_preset, BuiltinPreset::Player);
        assert!(draft.draft_hidden.is_empty());
        assert_eq!(modular.preset, builtin::MICRO_PRESET);
        assert_eq!(modular.hidden, BTreeSet::from(["deck-a".to_owned()]));
    }
}
