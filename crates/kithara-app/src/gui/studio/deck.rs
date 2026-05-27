use iced::{
    Alignment, Background, Border, Element, Length, Theme,
    font::Weight,
    widget::{
        Row, Space, button,
        button::{Status as ButtonStatus, Style as ButtonStyle},
        column, container,
        container::Style as ContainerStyle,
        row, text,
    },
};

use super::tokens::{StudioRadius, StudioSize, StudioSpace, StudioType};
use crate::{
    gui::{
        app::Kithara,
        fonts,
        icons::Icon,
        message::Message,
        tokens::Gap,
        view::{eq_band_label, format_time, track_subtitle, with_alpha},
        widgets,
    },
    theme::gui::GuiPalette,
};

/// Deck EQ knob range in dB; mirrors the compact equalizer tab so a
/// band's knob and slider agree on the same gain.
const EQ_MIN_DB: f32 = -24.0;
const EQ_MAX_DB: f32 = 6.0;

pub(super) fn view_deck(state: &Kithara) -> Element<'_, Message> {
    let p = state.palette;

    container(
        column![
            deck_header(state, p),
            waveform_cluster(state, p),
            transport_row(state, p),
            knob_strip(state, p),
        ]
        .spacing(Gap::CONTENT),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .padding(StudioSpace::DECK)
    .style(deck_style(p))
    .into()
}

/// One rotary knob per EQ band, frequency-labelled, sharing the band gain
/// with the compact equalizer tab through `EqBandChanged`.
fn knob_strip(state: &Kithara, p: GuiPalette) -> Element<'static, Message> {
    let total = state.ui_state.eq_bands.len();
    let strip = state
        .ui_state
        .eq_bands
        .iter()
        .copied()
        .enumerate()
        .map(|(index, value)| knob_cell(index, total, value, p))
        .fold(
            row![].spacing(Gap::INLINE).align_y(Alignment::Center),
            Row::push,
        );

    container(strip)
        .width(Length::Fill)
        .padding([8.0, 14.0])
        .style(transport_style(p))
        .into()
}

fn knob_cell(index: usize, total: usize, value: f32, p: GuiPalette) -> Element<'static, Message> {
    let value = value.clamp(EQ_MIN_DB, EQ_MAX_DB);

    container(
        column![
            widgets::knob(
                value,
                EQ_MIN_DB,
                EQ_MAX_DB,
                StudioSize::KNOB_SIZE,
                p,
                move |v| { Message::EqBandChanged(index, v) }
            ),
            text(eq_band_label(index, total))
                .size(StudioType::MONO_XS)
                .font(fonts::mono(Weight::Medium))
                .color(p.muted),
            text(format!("{value:+.1} dB"))
                .size(StudioType::MONO_SM)
                .font(fonts::mono(Weight::Medium))
                .color(p.accent),
        ]
        .align_x(Alignment::Center)
        .spacing(4.0),
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
                .size(StudioType::TRACK)
                .font(fonts::display(Weight::Semibold))
                .color(p.text),
            text(track_subtitle(state))
                .size(StudioType::BODY_SM)
                .font(fonts::SANS)
                .color(p.text_dim),
        ]
        .spacing(Gap::TEXT_STACK),
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
            StudioSize::WAVEFORM_HEIGHT,
            p,
        ),
        _ => Space::new()
            .width(Length::Fill)
            .height(Length::Fixed(StudioSize::WAVEFORM_HEIGHT))
            .into(),
    };

    column![
        container(canvas)
            .width(Length::Fill)
            .height(Length::Fixed(StudioSize::WAVEFORM_HEIGHT))
            .style(waveform_style(p)),
        row![
            text(current)
                .size(StudioType::MONO_SM)
                .font(fonts::MONO)
                .color(p.muted),
            Space::new().width(Length::Fill),
            text(total)
                .size(StudioType::MONO_SM)
                .font(fonts::MONO)
                .color(p.muted),
        ]
        .align_y(Alignment::Center),
    ]
    .spacing(Gap::INLINE_TIGHT)
    .into()
}

/// Playhead fraction in `[0, 1]`. `None`/zero duration reads as 0.
fn playhead_progress(position: f64, duration: f64) -> f32 {
    if duration <= 0.0 {
        return 0.0;
    }
    #[expect(
        clippy::cast_possible_truncation,
        reason = "playhead fraction is bounded to [0,1]; f32 precision is ample for a pixel offset"
    )]
    let progress = (position / duration).clamp(0.0, 1.0) as f32;
    progress
}

fn transport_row(state: &Kithara, p: GuiPalette) -> Element<'_, Message> {
    container(
        row![
            secondary_transport_button(Icon::SkipPrev, p, Message::Prev),
            widgets::play_button(state.ui_state.playing, p, Message::TogglePlayPause),
            secondary_transport_button(Icon::SkipNext, p, Message::Next),
        ]
        .align_y(Alignment::Center)
        .spacing(Gap::INLINE),
    )
    .width(Length::Fill)
    .center_x(Length::Fill)
    .padding([8.0, 14.0])
    .style(transport_style(p))
    .into()
}

fn secondary_transport_button(
    icon: Icon,
    p: GuiPalette,
    message: Message,
) -> Element<'static, Message> {
    const SIZE: f32 = 44.0;

    button(
        container(icon.view(StudioSize::TRANSPORT_ICON, p.text_dim))
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
            Some(Background::Color(iced::Color::TRANSPARENT))
        }
    };

    ButtonStyle {
        background,
        text_color: p.text_dim,
        border: Border::default().rounded(StudioRadius::BUTTON),
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
                    .rounded(StudioRadius::BUTTON)
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
                    .rounded(StudioRadius::SURFACE)
                    .width(1.0)
                    .color(p.line),
            )
    }
}
