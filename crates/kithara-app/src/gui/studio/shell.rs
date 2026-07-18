use iced::{
    Element, Length,
    widget::{column, container, row},
};

use super::{
    deck::view_deck,
    library::view_library,
    status::view_status_bar,
    styles::{horizontal_divider, shell_style, vertical_divider},
    tokens::studio_size,
    topbar::view_topbar,
};
use crate::gui::{app::Kithara, message::Message};

pub(crate) fn view_dj_studio(state: &Kithara) -> Element<'_, Message> {
    let p = state.palette;

    container(
        column![
            view_topbar(p),
            horizontal_divider(studio_size::DIVIDER, p.line),
            row![
                view_deck(state),
                vertical_divider(studio_size::DIVIDER, f32::INFINITY, p.line),
                view_library(state),
            ]
            .width(Length::Fill)
            .height(Length::Fill),
            horizontal_divider(studio_size::DIVIDER, p.line_soft),
            view_status_bar(state),
        ]
        .width(Length::Fill)
        .height(Length::Fill),
    )
    .height(Length::Fill)
    .style(shell_style(p))
    .into()
}
