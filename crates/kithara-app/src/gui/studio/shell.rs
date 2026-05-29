use iced::{
    Element, Length,
    widget::{column, container, row},
};

use super::{
    deck::view_deck,
    library::view_library,
    status::view_status_bar,
    styles::{shell_style, vertical_divider},
    tokens::StudioSize,
    topbar::view_topbar,
};
use crate::gui::{app::Kithara, message::Message};

pub(crate) fn view_dj_studio(state: &Kithara) -> Element<'_, Message> {
    let p = state.palette;

    container(
        column![
            view_topbar(p),
            row![
                view_deck(state),
                vertical_divider(StudioSize::DIVIDER, f32::INFINITY, p.line),
                view_library(state),
            ]
            .width(Length::Fill)
            .height(Length::Fill),
            view_status_bar(state),
        ]
        .width(Length::Fill)
        .height(Length::Fill),
    )
    .height(Length::Fill)
    .style(shell_style(p))
    .into()
}
