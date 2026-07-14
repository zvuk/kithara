use iced::{
    Alignment, Border, Color, Element, Length, Theme,
    font::Weight,
    widget::{Space, container, container::Style as ContainerStyle, row, text},
};
use kithara::prelude::EngineLoadSnapshot;

use super::tokens::{studio_radius, studio_size, studio_space, studio_type};
use crate::{
    gui::{app::Kithara, fonts, message::Message, tokens::gap, view::mix_colors},
    theme::gui::GuiPalette,
};

pub(super) fn view_status_bar(state: &Kithara) -> Element<'static, Message> {
    let p = state.palette;
    let playing = state.ui_state.playing;
    let track_count = state.ui_state.tracks.len();
    let load = state.ui_state.engine_load;
    // The engine readout (and its separator) appear only while a track is
    // actually producing; otherwise the section is omitted entirely.
    let show_load = playing && load.is_active();

    let dot_color = if playing { p.success } else { p.muted };
    let play_label = if playing { "Playing" } else { "Paused" };

    let mut bar = row![
        status_dot(dot_color),
        status_text(play_label.to_string(), p),
        separator_text(p),
        status_text(format!("{track_count} tracks"), p),
    ]
    .align_y(Alignment::Center)
    .spacing(gap::SECTION);

    if show_load {
        bar = bar.push(separator_text(p)).push(engine_load_view(load, p));
    }
    bar = bar.push(Space::new().width(Length::Fill));

    container(bar)
        .padding(studio_space::STATUS)
        .style(status_style(p))
        .into()
}

/// Audio-engine cost readout shown inline in the status bar.
fn engine_load_view(load: EngineLoadSnapshot, p: GuiPalette) -> Element<'static, Message> {
    let pct = (load.load() * 100.0).clamp(0.0, 999.0);
    row![
        status_text("load".to_string(), p),
        text(format!("{pct:.0}% \u{00b7} {:.1}ms", load.ms()))
            .size(studio_type::MONO_SM)
            .font(fonts::mono(Weight::Semibold))
            .color(load_color(load.load(), p)),
    ]
    .align_y(Alignment::Center)
    .spacing(gap::INLINE)
    .into()
}

/// Traffic-light color for an engine load fraction.
fn load_color(load: f32, p: GuiPalette) -> Color {
    let base = if load < 0.33 {
        p.success
    } else if load < 0.66 {
        p.warning
    } else {
        p.danger
    };
    mix_colors(base, p.muted, 0.4)
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
