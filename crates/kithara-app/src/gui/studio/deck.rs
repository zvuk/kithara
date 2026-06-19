use iced::{
    Alignment, Background, Border, Color, Element, Length, Padding, Theme,
    alignment::{Horizontal, Vertical},
    font::Weight,
    widget::{
        Row, Space, button,
        button::{Status as ButtonStatus, Style as ButtonStyle},
        column, container,
        container::Style as ContainerStyle,
        row, stack, text,
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
        dj::DjMsg,
        fonts,
        icons::Icon,
        message::Message,
        tokens::gap,
        view::{eq_band_label, format_time, track_subtitle, with_alpha},
        widgets,
        widgets::WaveMsg,
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
            super::timestretch::view_timestretch_panel(state),
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
                widgets::VFaderParams {
                    value,
                    min: EQ_MIN_DB,
                    max: EQ_MAX_DB,
                    height: studio_size::FADER_HEIGHT,
                },
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

    let wave = state
        .ui_state
        .analysis
        .as_ref()
        .and_then(|analysis| analysis.waveform.as_ref())
        .filter(|wave| !wave.is_empty());

    let canvas: Element<'_, Message> = wave.map_or_else(
        || {
            Space::new()
                .width(Length::Fill)
                .height(Length::Fixed(studio_size::WAVEFORM_HEIGHT))
                .into()
        },
        |wave| {
            widgets::waveform(
                wave.clone(),
                widgets::BeatMarks {
                    beats: state.ui_state.beat_marks.clone(),
                    downbeats: state.ui_state.downbeat_marks.clone(),
                },
                progress,
                duration,
                state.dj.wave,
                studio_size::WAVEFORM_HEIGHT,
                p,
            )
        },
    );

    let wave_box = container(canvas)
        .width(Length::Fill)
        .height(Length::Fixed(studio_size::WAVEFORM_HEIGHT))
        .style(waveform_style(p));

    let wave_layer: Element<'_, Message> = if wave.is_some() {
        stack![wave_box, zoom_control(state.dj.wave.zoom(), p)].into()
    } else {
        wave_box.into()
    };

    column![
        wave_layer,
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

fn zoom_control(zoom: f32, p: GuiPalette) -> Element<'static, Message> {
    let control = row![
        zoom_button("\u{2212}", WaveMsg::Zoom(1.0 / widgets::ZOOM_STEP), p),
        text(format!("{zoom:.1}\u{00d7}"))
            .size(studio_type::MONO_SM)
            .font(fonts::MONO)
            .color(p.text_dim),
        zoom_button("+", WaveMsg::Zoom(widgets::ZOOM_STEP), p),
    ]
    .spacing(gap::INLINE_TIGHT)
    .align_y(Alignment::Center);

    container(
        container(control)
            .padding([2.0, 6.0])
            .style(zoom_pill_style(p)),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .align_x(Horizontal::Right)
    .align_y(Vertical::Top)
    .padding(Padding::from(8.0))
    .into()
}

fn zoom_button(label: &'static str, msg: WaveMsg, p: GuiPalette) -> Element<'static, Message> {
    const SIZE: f32 = 20.0;

    button(
        container(text(label).size(studio_type::BODY_MD).color(p.text))
            .center_x(Length::Fill)
            .center_y(Length::Fill),
    )
    .width(Length::Fixed(SIZE))
    .height(Length::Fixed(SIZE))
    .padding(0)
    .style(move |_theme: &Theme, status| zoom_button_style(p, status))
    .on_press(Message::Dj(DjMsg::Wave(msg)))
    .into()
}

fn zoom_pill_style(p: GuiPalette) -> impl Fn(&Theme) -> ContainerStyle {
    move |_theme| {
        ContainerStyle::default()
            .background(Background::Color(with_alpha(p.bg_deep, 0.7)))
            .border(
                Border::default()
                    .rounded(studio_radius::SM)
                    .width(1.0)
                    .color(p.line),
            )
    }
}

fn zoom_button_style(p: GuiPalette, status: ButtonStatus) -> ButtonStyle {
    let background = match status {
        ButtonStatus::Hovered => Some(Background::Color(with_alpha(p.bg_panel_2, 0.7))),
        ButtonStatus::Pressed => Some(Background::Color(with_alpha(p.bg_panel_2, 0.5))),
        ButtonStatus::Active | ButtonStatus::Disabled => {
            Some(Background::Color(Color::TRANSPARENT))
        }
    };

    ButtonStyle {
        background,
        text_color: p.text,
        border: Border::default().rounded(studio_radius::SM),
        ..ButtonStyle::default()
    }
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
