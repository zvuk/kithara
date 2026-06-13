use iced::{
    Alignment, Background, Border, Color, Element, Length, Theme,
    font::Weight,
    widget::{
        Row, Space, button,
        button::{Status as ButtonStatus, Style as ButtonStyle},
        column, container,
        container::Style as ContainerStyle,
        row, text,
    },
};
use num_traits::cast::AsPrimitive;

use super::{
    styles::vertical_divider,
    tokens::{studio_radius, studio_size, studio_space, studio_type},
};
use crate::{
    gui::{
        app::Kithara,
        fonts,
        icons::Icon,
        message::Message,
        tokens::gap,
        view::{eq_band_label, format_time, track_subtitle, with_alpha},
        widgets,
    },
    theme::gui::GuiPalette,
};

mod consts {
    /// Deck EQ fader range in dB; mirrors the compact equalizer tab so a
    /// band's fader and slider agree on the same gain.
    pub(super) const EQ_MIN_DB: f32 = -24.0;
    pub(super) const EQ_MAX_DB: f32 = 6.0;
    /// Gap between adjacent EQ faders; kept tight to match the reference deck.
    pub(super) const FADER_GAP: f32 = 2.0;
    /// Vertical gap between a fader track and its value/frequency labels.
    pub(super) const FADER_LABEL_GAP: f32 = 2.0;
    /// Vertical inset of the fader bank from the panel's top/bottom border.
    pub(super) const FADER_EDGE_PAD: f32 = 10.0;
    /// Gap between the transport cluster and the fader bank.
    pub(super) const TRANSPORT_FADERS_GAP: f32 = 14.0;
}
use consts::*;

pub(super) fn view_deck(state: &Kithara) -> Element<'_, Message> {
    let p = state.palette;

    container(
        column![
            deck_header(state, p),
            waveform_cluster(state, p),
            transport_and_faders_row(state, p),
        ]
        .spacing(gap::CONTENT),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .padding(studio_space::DECK)
    .style(deck_style(p))
    .into()
}

/// One vertical fader per EQ band, frequency-labelled, sharing the band gain
/// with the compact equalizer tab through `EqBandChanged`. Bands fill the
/// space right of the transport with a tight gap, mirroring the reference.
fn faders(state: &Kithara, p: GuiPalette) -> Element<'static, Message> {
    let total = state.ui_state.eq_bands.len();
    let strip = state
        .ui_state
        .eq_bands
        .iter()
        .copied()
        .enumerate()
        .map(|(index, value)| fader_cell(index, total, value, p))
        .fold(
            row![]
                .spacing(FADER_GAP)
                .align_y(Alignment::End)
                .width(Length::Fill),
            Row::push,
        );

    container(strip)
        .width(Length::Fill)
        .padding([FADER_EDGE_PAD, 0.0])
        .into()
}

fn fader_cell(index: usize, total: usize, value: f32, p: GuiPalette) -> Element<'static, Message> {
    let value = value.clamp(EQ_MIN_DB, EQ_MAX_DB);

    container(
        column![
            text(format!("{value:+.0}"))
                .size(studio_type::MONO_SM)
                .line_height(1.0)
                .font(fonts::mono(Weight::Medium))
                .color(p.text_dim),
            widgets::vfader(
                index,
                value,
                EQ_MIN_DB,
                EQ_MAX_DB,
                studio_size::FADER_HEIGHT,
                p,
            ),
            text(eq_band_label(index, total))
                .size(studio_type::MONO_XS)
                .line_height(1.0)
                .font(fonts::mono(Weight::Medium))
                .color(p.text_dim),
        ]
        .align_x(Alignment::Center)
        .spacing(FADER_LABEL_GAP),
    )
    .width(Length::FillPortion(1))
    .into()
}

fn deck_header(state: &Kithara, p: GuiPalette) -> Element<'_, Message> {
    let title = if state.ui_state.track_name.trim().is_empty() {
        "No track loaded"
    } else {
        state.ui_state.track_name.as_str()
    };

    container(
        column![
            text(title)
                .size(studio_type::TRACK)
                .font(fonts::display(Weight::Semibold))
                .color(p.text),
            text(track_subtitle(state))
                .size(studio_type::BODY_SM)
                .font(fonts::SANS)
                .color(p.text_dim),
        ]
        .spacing(gap::TEXT_STACK),
    )
    .width(Length::Fill)
    .into()
}

fn waveform_cluster(state: &Kithara, p: GuiPalette) -> Element<'_, Message> {
    let duration = state.ui_state.duration.max(0.0);
    // While scrubbing the waveform, follow the seek target instead of the
    // engine position so the playhead and timer track the pointer.
    let head_position = if state.ui_state.is_seeking {
        state.ui_state.seek_position
    } else {
        state.ui_state.position
    };
    let current = format_time(head_position.max(0.0));
    let total = format_time(duration);
    let progress = playhead_progress(head_position, duration);

    let canvas: Element<'_, Message> = match state.ui_state.waveform.as_ref() {
        Some(env) if env.len() >= 2 => widgets::waveform(
            env.clone(),
            progress,
            duration,
            studio_size::WAVEFORM_HEIGHT,
            p,
        ),
        _ => Space::new()
            .width(Length::Fill)
            .height(Length::Fixed(studio_size::WAVEFORM_HEIGHT))
            .into(),
    };

    column![
        container(canvas)
            .width(Length::Fill)
            .height(Length::Fixed(studio_size::WAVEFORM_HEIGHT))
            .style(waveform_style(p)),
        row![
            text(current)
                .size(studio_type::MONO_SM)
                .font(fonts::MONO)
                .color(p.muted),
            Space::new().width(Length::Fill),
            text(total)
                .size(studio_type::MONO_SM)
                .font(fonts::MONO)
                .color(p.muted),
        ]
        .align_y(Alignment::Center),
    ]
    .spacing(gap::INLINE_TIGHT)
    .into()
}

/// Playhead fraction in `[0, 1]`. `None`/zero duration reads as 0.
fn playhead_progress(position: f64, duration: f64) -> f32 {
    if duration <= 0.0 {
        return 0.0;
    }
    let progress: f32 = (position / duration).clamp(0.0, 1.0).as_();
    progress
}

/// Transport (prev / play / next) on the left, a divider, then the EQ fader
/// bank - all in one panel, matching the reference deck layout.
fn transport_and_faders_row(state: &Kithara, p: GuiPalette) -> Element<'static, Message> {
    container(
        row![
            transport_buttons(state, p),
            vertical_divider(studio_size::DIVIDER, studio_size::FADER_HEIGHT, p.line_soft),
            faders(state, p),
        ]
        .align_y(Alignment::Center)
        .spacing(TRANSPORT_FADERS_GAP),
    )
    .width(Length::Fill)
    .padding([8.0, 14.0])
    .style(transport_style(p))
    .into()
}

fn transport_buttons(state: &Kithara, p: GuiPalette) -> Element<'static, Message> {
    row![
        secondary_transport_button(Icon::SkipPrev, p, Message::Prev),
        widgets::play_button(state.ui_state.playing, p, Message::TogglePlayPause),
        secondary_transport_button(Icon::SkipNext, p, Message::Next),
    ]
    .align_y(Alignment::Center)
    .spacing(gap::INLINE)
    .into()
}

fn secondary_transport_button(
    icon: Icon,
    p: GuiPalette,
    message: Message,
) -> Element<'static, Message> {
    const SIZE: f32 = 44.0;

    button(
        container(icon.view(studio_size::TRANSPORT_ICON, p.text_dim))
            .center_x(Length::Fill)
            .center_y(Length::Fill),
    )
    .width(Length::Fixed(SIZE))
    .height(Length::Fixed(SIZE))
    .padding(0)
    .style(move |_theme: &Theme, status| transport_secondary_style(p, status))
    .on_press(message)
    .into()
}

fn transport_secondary_style(p: GuiPalette, status: ButtonStatus) -> ButtonStyle {
    let background = match status {
        ButtonStatus::Hovered => Some(Background::Color(with_alpha(p.bg_panel_2, 0.6))),
        ButtonStatus::Pressed => Some(Background::Color(with_alpha(p.bg_panel_2, 0.45))),
        ButtonStatus::Active | ButtonStatus::Disabled => {
            Some(Background::Color(Color::TRANSPARENT))
        }
    };

    ButtonStyle {
        background,
        text_color: p.text_dim,
        border: Border::default().rounded(studio_radius::BUTTON),
        ..ButtonStyle::default()
    }
}

fn deck_style(p: GuiPalette) -> impl Fn(&Theme) -> ContainerStyle {
    move |_theme| {
        ContainerStyle::default()
            .background(Background::Color(with_alpha(p.bg_deep, 0.24)))
            .color(p.text)
    }
}

fn waveform_style(p: GuiPalette) -> impl Fn(&Theme) -> ContainerStyle {
    move |_theme| {
        ContainerStyle::default()
            .background(Background::Color(with_alpha(p.bg_deep, 0.55)))
            .border(
                Border::default()
                    .rounded(studio_radius::BUTTON)
                    .width(1.0)
                    .color(p.line),
            )
    }
}

fn transport_style(p: GuiPalette) -> impl Fn(&Theme) -> ContainerStyle {
    move |_theme| {
        ContainerStyle::default()
            .background(Background::Color(with_alpha(p.bg_deep, 0.52)))
            .border(
                Border::default()
                    .rounded(studio_radius::SURFACE)
                    .width(1.0)
                    .color(p.line),
            )
    }
}
