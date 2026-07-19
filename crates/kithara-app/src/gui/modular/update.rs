use iced::{Size, Task, window, window::Settings};
use kithara_ui::{
    builtin,
    compile::{CompiledUi, compile},
    render::UiEvent,
    source::UiConfig,
};
use tracing::error;

use super::{ModularView, dispatch, endpoints};
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
        UiEvent::CloseSettings => close_settings(state),
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
        &endpoints::catalog(),
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
    let settings = Settings {
        size: Size::new(420.0, 380.0),
        min_size: Some(Size::new(380.0, 320.0)),
        exit_on_close_request: false,
        ..Settings::default()
    };
    let (id, open) = window::open(settings);
    state.settings_window_id = Some(id);
    open.discard()
}

fn close_settings(state: &mut Kithara) -> Task<Message> {
    state
        .settings_window_id
        .take()
        .map_or_else(Task::none, window::close)
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;
    use kithara_ui::builtin;

    use super::{commit_preset, compile_preset};
    use crate::gui::modular::ModularView;

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
}
