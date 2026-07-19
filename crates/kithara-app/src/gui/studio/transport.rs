use iced::{
    Alignment, Background, Color, Element, Length, Theme,
    widget::{
        button,
        button::{Status as ButtonStatus, Style as ButtonStyle},
        container, row,
    },
};

use super::tokens::studio_size;
use crate::{
    gui::{icons::Icon, tokens::gap, widgets},
    theme::gui::GuiPalette,
};

#[derive(Debug, Clone, Copy)]
pub(super) enum TransportMsg {
    PlayPause,
    Next,
    Prev,
}

pub(super) fn view_transport(playing: bool, p: GuiPalette) -> Element<'static, TransportMsg> {
    row![
        secondary_button(Icon::SkipPrev, p, TransportMsg::Prev),
        widgets::play_button(playing, p, TransportMsg::PlayPause),
        secondary_button(Icon::SkipNext, p, TransportMsg::Next),
    ]
    .align_y(Alignment::Center)
    .spacing(gap::INLINE)
    .into()
}

fn secondary_button(
    icon: Icon,
    p: GuiPalette,
    message: TransportMsg,
) -> Element<'static, TransportMsg> {
    const SIZE: f32 = 44.0;

    button(
        container(icon.view(studio_size::TRANSPORT_ICON, p.text_dim))
            .center_x(Length::Fill)
            .center_y(Length::Fill),
    )
    .width(Length::Fixed(SIZE))
    .height(Length::Fixed(SIZE))
    .padding(0)
    .style(move |_theme: &Theme, status| secondary_style(p, status))
    .on_press(message)
    .into()
}

fn secondary_style(p: GuiPalette, status: ButtonStatus) -> ButtonStyle {
    let background = match status {
        ButtonStatus::Hovered | ButtonStatus::Pressed => Some(Background::Color(p.bg_elev)),
        ButtonStatus::Active | ButtonStatus::Disabled => {
            Some(Background::Color(Color::TRANSPARENT))
        }
    };

    ButtonStyle {
        background,
        text_color: p.text_dim,
        ..ButtonStyle::default()
    }
}
