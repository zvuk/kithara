use iced::Task;
use kithara_ui::{builtin, compile::compile, source::Limits};
use tracing::error;

use super::{ModularMsg, ViewMode, endpoints};
use crate::gui::{app::Kithara, message::Message};

pub(crate) fn update(state: &mut Kithara, message: ModularMsg) -> Task<Message> {
    match message {
        ModularMsg::Enter => {
            state.modular.preset = builtin::MICRO_PRESET.to_owned();
            match compile(
                builtin::MICRO_PRESET,
                &builtin::resolver(),
                &endpoints::catalog(),
                &endpoints::registry(),
                &Limits::default(),
            ) {
                Ok(compiled) => {
                    state.modular.compiled = Some(compiled);
                    state.modular.error = None;
                    state.view_mode = ViewMode::Modular;
                }
                Err(error) => {
                    error!(
                        error = %error,
                        preset = builtin::MICRO_PRESET,
                        "modular preset compile failed"
                    );
                    state.modular.compiled = None;
                    state.modular.error = Some(error.to_string());
                }
            }
        }
        ModularMsg::Exit => {
            state.view_mode = ViewMode::Compact;
            state.dj.open = false;
        }
    }
    Task::none()
}
