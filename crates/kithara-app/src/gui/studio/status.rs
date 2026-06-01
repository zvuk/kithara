use iced::{
    Alignment, Border, Color, Element, Length, Theme,
    font::Weight,
    widget::{Space, container, container::Style as ContainerStyle, row, text},
};

use super::tokens::{studio_radius, studio_size, studio_space, studio_type};
use crate::{
    gui::{app::Kithara, fonts, message::Message, tokens::gap},
    theme::gui::GuiPalette,
};

pub(super) fn view_status_bar(state: &Kithara) -> Element<'static, Message> {
    let p = state.palette;
    let playing = state.ui_state.playing;
    let track_count = state.ui_state.tracks.len();

    let dot_color = if playing { p.success } else { p.muted };
    let play_label = if playing { "Playing" } else { "Paused" };

    container(
        row![
            status_dot(dot_color),
            status_text(play_label.to_string(), p),
            separator_text(p),
            status_text(format!("{track_count} tracks"), p),
            Space::new().width(Length::Fill),
        ]
        .align_y(Alignment::Center)
        .spacing(gap::SECTION),
    )
    .padding(studio_space::STATUS)
    .style(status_style(p))
    .into()
}

fn status_text(label: String, p: GuiPalette) -> Element<'static, Message> {
    text(label)
        .size(studio_type::MONO_SM)
        .font(fonts::mono(Weight::Medium))
        .color(p.muted)
        .into()
}

fn separator_text(p: GuiPalette) -> Element<'static, Message> {
    text("|")
        .size(studio_type::MONO_SM)
        .font(fonts::mono(Weight::Medium))
        .color(p.line)
        .into()
}

fn status_dot(color: Color) -> Element<'static, Message> {
    container(Space::new())
        .width(Length::Fixed(studio_size::STATUS_DOT))
        .height(Length::Fixed(studio_size::STATUS_DOT))
        .style(move |_theme: &Theme| {
            ContainerStyle::default()
                .background(color)
                .border(Border::default().rounded(studio_radius::ROUND))
        })
        .into()
}

fn status_style(p: GuiPalette) -> impl Fn(&Theme) -> ContainerStyle {
    move |_theme| {
        ContainerStyle::default()
            .background(p.bg_deep.scale_alpha(0.85))
            .color(p.muted)
            .border(Border::default().width(1.0).color(p.line))
    }
}
