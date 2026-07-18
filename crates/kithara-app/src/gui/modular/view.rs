use iced::{
    Element,
    widget::{button, column, text},
    window,
};

use super::{super::app::Kithara, ModularMsg};
use crate::gui::message::Message;

pub(crate) fn render(state: &Kithara, _window: window::Id) -> Element<'_, Message> {
    let status: Element<'_, Message> = match (&state.modular.compiled, &state.modular.error) {
        (Some(_), _) => text(format!("modular preset: {}", state.modular.preset)).into(),
        (None, Some(error)) => text(format!("preset error: {error}")).into(),
        (None, None) => text("no preset loaded").into(),
    };
    column![
        status,
        button(text("Exit")).on_press(Message::Modular(ModularMsg::Exit)),
    ]
    .spacing(8)
    .padding(12)
    .into()
}
