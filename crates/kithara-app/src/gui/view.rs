use iced::{Element, window};

use super::{app::Kithara, message::Message, modular};

pub(crate) fn view(state: &Kithara, window_id: window::Id) -> Element<'_, Message> {
    if state.settings_window_id == Some(window_id) {
        modular::render_settings(state)
    } else {
        modular::view::render(state)
    }
}
