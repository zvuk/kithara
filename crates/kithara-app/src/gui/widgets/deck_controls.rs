use iced::{
    Alignment, Background, Border, Element, Length,
    alignment::Vertical,
    font::Weight,
    widget::{Row, Space, column, container, container::Style as ContainerStyle, row},
};
use kithara::audio::BeatGrid;
use num_traits::ToPrimitive;

use crate::{
    gui::{
        app::Kithara,
        fonts,
        message::Message,
        modular::ReadValue,
        tokens::{chrome, deck, gap, telemetry, transport},
        typography::shaped_text,
    },
    theme::gui::GuiPalette,
    waveform::TrackAnalysis,
};

struct Consts;

impl Consts {
    const CONFIGURED_SOURCE: &'static str = "configured source";
    const EM_DASH: &'static str = "\u{2014}";
    const HLS_SOURCE: &'static str = "HLS stream";
    const MICRO_STYLE: &'static str = "micro";
    const NO_SOURCE: &'static str = "no source";
    const NO_TRACK: &'static str = "No track loaded";
    const SECONDS_PER_MINUTE: u64 = 60;
}

pub(crate) fn header<'a>(
    state: &'a Kithara,
    badge: Option<&'a str>,
    value: &Option<ReadValue<'a>>,
) -> Element<'a, Message> {
    let Some(badge) = badge else {
        return Space::new().into();
    };
    let p = state.palette;
    let analysis = waveform_value(value);
    let art = container(
        shaped_text("ART")
            .font(fonts::MONO)
            .size(deck::ART_LABEL_SIZE)
            .color(p.canvas.muted),
    )
    .center(deck::ART_SIZE)
    .style(move |_| {
        ContainerStyle::default()
            .background(Background::Color(p.canvas.bg_panel))
            .border(
                Border::default()
                    .width(deck::ART_BORDER_WIDTH)
                    .color(p.canvas.line),
            )
    });
    let summary = column![
        shaped_text(track_title(state))
            .font(fonts::display(Weight::Semibold))
            .size(deck::TITLE_SIZE)
            .color(p.canvas.text),
        shaped_text(source_kind(state))
            .font(fonts::SANS)
            .size(deck::ARTIST_SIZE)
            .color(p.canvas.text_dim),
    ]
    .spacing(gap::GRID)
    .width(Length::Fill);
    let badge = container(
        shaped_text(badge)
            .font(fonts::display(Weight::Bold))
            .size(deck::BADGE_TEXT)
            .color(p.canvas.bg),
    )
    .center(deck::BADGE_SIZE)
    .style(move |_| ContainerStyle::default().background(Background::Color(p.accent)));

    let mut children: Vec<Element<'a, Message>> = vec![
        art.into(),
        summary.into(),
        Space::new().width(Length::Fill).into(),
    ];
    if let Some(bpm) = analysis_bpm(analysis) {
        children.push(readout_cell(
            p,
            "BPM",
            format!("{bpm:.1}"),
            deck::BPM_CELL_WIDTH,
        ));
    }
    children.push(readout_cell(
        p,
        "REMAIN",
        format!("-{}", format_time(remaining(state))),
        deck::REMAIN_CELL_WIDTH,
    ));
    children.push(badge.into());

    container(
        Row::with_children(children)
            .spacing(deck::HEADER_GAP)
            .align_y(Alignment::Center)
            .width(Length::Fill),
    )
    .padding([deck::HEADER_PADDING_Y, deck::HEADER_PADDING_X])
    .height(Length::Fixed(deck::HEADER_HEIGHT))
    .width(Length::Fill)
    .style(move |_| ContainerStyle::default().background(Background::Color(p.canvas.bg_deep)))
    .into()
}

pub(crate) fn summary<'a>(
    state: &'a Kithara,
    style: Option<&str>,
    value: &Option<ReadValue<'a>>,
) -> Element<'a, Message> {
    let p = state.palette;
    let title = match value {
        Some(ReadValue::Text(value)) if !value.is_empty() => *value,
        _ => track_title(state),
    };
    let compact = style == Some(Consts::MICRO_STYLE);
    let content: Element<'a, Message> = if compact {
        row![
            shaped_text(title)
                .font(fonts::display(Weight::Medium))
                .size(deck::MICRO_TITLE_SIZE)
                .color(p.canvas.text),
            shaped_text(source_kind(state))
                .font(fonts::SANS)
                .size(deck::MICRO_SOURCE_SIZE)
                .color(p.canvas.muted),
        ]
        .spacing(deck::MICRO_SUMMARY_GAP)
        .align_y(Alignment::Center)
        .into()
    } else {
        column![
            shaped_text(title)
                .font(fonts::display(Weight::Semibold))
                .size(deck::TITLE_SIZE)
                .color(p.canvas.text),
            shaped_text(source_kind(state))
                .font(fonts::SANS)
                .size(deck::ARTIST_SIZE)
                .color(p.canvas.text_dim),
        ]
        .spacing(gap::GRID)
        .into()
    };

    container(content)
        .height(Length::Fixed(deck::SUMMARY_HEIGHT))
        .width(Length::FillPortion(deck::SUMMARY_FILL_PORTION))
        .padding([0.0, deck::SUMMARY_PADDING_X])
        .align_y(Vertical::Center)
        .style(move |_| ContainerStyle::default().background(Background::Color(p.canvas.bg_panel)))
        .into()
}

pub(crate) fn bpm<'a>(
    state: &'a Kithara,
    fallback: Option<&str>,
    value: &Option<ReadValue<'a>>,
) -> Element<'a, Message> {
    let p = state.palette;
    let content: Element<'a, Message> = if let Some(bpm) = analysis_bpm(waveform_value(value)) {
        shaped_text(format!("{bpm:.1}"))
            .font(fonts::MONO)
            .size(telemetry::BPM_TEXT_SIZE)
            .color(p.accent_strong)
            .into()
    } else if fallback == Some("time") {
        column![
            shaped_text("TIME")
                .font(fonts::MONO)
                .size(deck::READOUT_LABEL_SIZE)
                .color(p.canvas.muted),
            shaped_text(format_time(state.ui_state.position))
                .font(fonts::MONO)
                .size(telemetry::BPM_TEXT_SIZE)
                .color(p.accent_strong),
        ]
        .spacing(gap::GRID)
        .align_x(Alignment::Center)
        .into()
    } else {
        return Space::new().into();
    };

    container(content)
        .width(Length::Fill)
        .height(Length::Fill)
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .style(move |_| ContainerStyle::default().background(Background::Color(p.canvas.bg_panel)))
        .into()
}

pub(crate) fn time<'a>(state: &'a Kithara, value: &Option<ReadValue<'_>>) -> Element<'a, Message> {
    if !matches!(value, Some(ReadValue::Scalar(_))) {
        return Space::new().into();
    }
    let p = state.palette;
    container(
        shaped_text(format!(
            "{} / {}",
            format_time(state.ui_state.position),
            format_time(state.ui_state.duration)
        ))
        .font(fonts::MONO)
        .size(transport::TIME_TEXT)
        .color(p.accent_strong),
    )
    .padding([0.0, transport::TIME_PADDING_X])
    .center_y(Length::Fill)
    .center_x(Length::Fill)
    .height(Length::Fixed(transport::BUTTON_HEIGHT))
    .width(Length::Fixed(transport::TIME_WIDTH))
    .style(move |_| ContainerStyle::default().background(Background::Color(p.canvas.bg_deep)))
    .into()
}

pub(crate) fn format_time(seconds: f64) -> String {
    let total = seconds.max(0.0).floor().to_u64().unwrap_or(0);
    let minutes = total / Consts::SECONDS_PER_MINUTE;
    let seconds = total % Consts::SECONDS_PER_MINUTE;
    format!("{minutes}:{seconds:02}")
}

fn readout_cell(
    p: GuiPalette,
    caption: &'static str,
    value: String,
    width: f32,
) -> Element<'static, Message> {
    container(
        column![
            shaped_text(caption)
                .font(fonts::MONO)
                .size(deck::READOUT_LABEL_SIZE)
                .color(p.canvas.muted),
            shaped_text(value)
                .font(fonts::MONO)
                .size(deck::READOUT_VALUE_SIZE)
                .color(p.accent_strong),
        ]
        .spacing(gap::GRID)
        .align_x(Alignment::End),
    )
    .padding([deck::TELEMETRY_PADDING_Y, deck::TELEMETRY_PADDING_X])
    .height(Length::Fixed(deck::READOUT_HEIGHT))
    .width(Length::Fixed(width))
    .center_y(Length::Fill)
    .style(move |_| {
        ContainerStyle::default()
            .background(Background::Color(p.canvas.bg_inset))
            .border(
                Border::default()
                    .width(chrome::BORDER_WIDTH)
                    .color(p.canvas.line),
            )
    })
    .into()
}

fn analysis_bpm(analysis: Option<&TrackAnalysis>) -> Option<f64> {
    analysis
        .and_then(TrackAnalysis::beat)
        .map(BeatGrid::bpm)
        .filter(|bpm| bpm.is_finite() && *bpm > 0.0)
}

fn waveform_value<'a>(value: &Option<ReadValue<'a>>) -> Option<&'a TrackAnalysis> {
    match value {
        Some(ReadValue::Waveform(analysis)) => *analysis,
        _ => None,
    }
}

fn remaining(state: &Kithara) -> f64 {
    (state.ui_state.duration - state.ui_state.position).max(0.0)
}

fn track_title(state: &Kithara) -> &str {
    if state.ui_state.track_name.is_empty() {
        Consts::NO_TRACK
    } else {
        &state.ui_state.track_name
    }
}

fn source_kind(state: &Kithara) -> &'static str {
    if !state.ui_state.variant_label.is_empty() {
        return Consts::HLS_SOURCE;
    }
    let entry = state
        .ui_state
        .current_track_index
        .and_then(|index| state.ui_state.tracks.get(index));
    match entry.and_then(|track| track.url.as_deref()) {
        Some(url) if url.contains(".m3u8") => Consts::HLS_SOURCE,
        Some(url) if url.starts_with("http://") || url.starts_with("https://") => "network stream",
        Some(_) => "local file",
        None if entry.is_some() => Consts::CONFIGURED_SOURCE,
        _ if state.ui_state.track_name.is_empty() => Consts::EM_DASH,
        _ => Consts::NO_SOURCE,
    }
}
