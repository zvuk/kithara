use iced::{
    Alignment, Background, Border, Element, Length, Shadow, Theme, Vector,
    font::Weight,
    widget::{
        Row, Space, button,
        button::{Status as ButtonStatus, Style as ButtonStyle},
        column, container,
        container::Style as ContainerStyle,
        row, text,
    },
};

use super::{
    styles::primary_button_background,
    tokens::{Gap, StudioRadius, StudioSize, StudioSpace, StudioType},
};
use crate::{
    gui::{
        app::Kithara,
        fonts,
        icons::Icon,
        message::Message,
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
    let current = format_time(state.ui_state.position.max(0.0));
    let total = format_time(state.ui_state.duration.max(0.0));
    let progress = playhead_progress(state.ui_state.position, state.ui_state.duration);

    let canvas: Element<'_, Message> = match state.ui_state.waveform.as_ref() {
        Some(env) if env.len() >= 2 => {
            widgets::waveform(env.clone(), progress, StudioSize::WAVEFORM_HEIGHT, p)
        }
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
            icon_transport_button(
                Icon::SkipPrev,
                StudioSize::TRANSPORT_ICON,
                p,
                false,
                Message::Prev
            ),
            icon_transport_button(
                if state.ui_state.playing {
                    Icon::Pause
                } else {
                    Icon::Play
                },
                StudioSize::TRANSPORT_ICON_LG,
                p,
                true,
                Message::TogglePlayPause,
            ),
            icon_transport_button(
                Icon::SkipNext,
                StudioSize::TRANSPORT_ICON,
                p,
                false,
                Message::Next
            ),
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

fn icon_transport_button(
    icon: Icon,
    icon_size: f32,
    p: GuiPalette,
    primary: bool,
    message: Message,
) -> Element<'static, Message> {
    let sz = if primary { 56.0 } else { 44.0 };
    let icon_color = if primary { p.bg } else { p.text_dim };

    button(
        container(icon.view(icon_size, icon_color))
            .center_x(Length::Fill)
            .center_y(Length::Fill),
    )
    .width(Length::Fixed(sz))
    .height(Length::Fixed(sz))
    .padding(0)
    .style(move |_theme: &Theme, status| {
        if primary {
            transport_primary_style(p, status)
        } else {
            transport_secondary_style(p, status)
        }
    })
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

fn transport_primary_style(p: GuiPalette, status: ButtonStatus) -> ButtonStyle {
    let background = match status {
        ButtonStatus::Pressed => Some(Background::Color(with_alpha(p.accent, 0.85))),
        ButtonStatus::Disabled => Some(Background::Color(with_alpha(p.accent, 0.4))),
        ButtonStatus::Active | ButtonStatus::Hovered => Some(primary_button_background(p)),
    };

    let shadow = if matches!(status, ButtonStatus::Hovered) {
        Shadow {
            color: p.accent_glow,
            offset: Vector::new(0.0, 0.0),
            blur_radius: 12.0,
        }
    } else {
        Shadow::default()
    };

    ButtonStyle {
        background,
        text_color: p.bg,
        border: Border::default().rounded(StudioRadius::BUTTON),
        shadow,
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
