use iced::{
    Alignment, Background, Border, Element, Length,
    alignment::Vertical,
    font::Weight,
    widget::{Row, Space, column, container, container::Style as ContainerStyle, row},
};
use num_traits::ToPrimitive;

use super::chrome;
use crate::{
    module::DeckSummaryStyle,
    render::{ReadValue, Reads, RenderPalette, UiEvent, WaveformView, fonts, shaped_text},
};

struct Consts;

impl Consts {
    const ART_BORDER_WIDTH: f32 = 1.0;
    const ART_LABEL_SIZE: f32 = 7.0;
    const ART_SIZE: f32 = 40.0;
    const ARTIST_SIZE: f32 = 13.0;
    const BADGE_SIZE: f32 = 26.0;
    const BADGE_TEXT: f32 = 14.0;
    const BPM_CELL_WIDTH: f32 = 64.0;
    const BPM_TEXT_SIZE: f32 = 11.0;
    const EM_DASH: &'static str = "\u{2014}";
    const HEADER_GAP: f32 = 12.0;
    const HEADER_HEIGHT: f32 = 60.0;
    const HEADER_PADDING_X: f32 = 12.0;
    const HEADER_PADDING_Y: f32 = 9.0;
    const MICRO_SOURCE_SIZE: f32 = 13.0;
    const MICRO_SUMMARY_GAP: f32 = 7.0;
    const MICRO_TITLE_SIZE: f32 = 14.0;
    const NO_SOURCE: &'static str = "no source";
    const NO_TRACK: &'static str = "No track loaded";
    const READOUT_GAP: f32 = 1.0;
    const READOUT_HEIGHT: f32 = 60.0;
    const READOUT_LABEL_SIZE: f32 = 9.0;
    const READOUT_VALUE_SIZE: f32 = 13.0;
    const REMAIN_CELL_WIDTH: f32 = 72.0;
    const SECONDS_PER_MINUTE: u64 = 60;
    const SUMMARY_FILL_PORTION: u16 = 3;
    const SUMMARY_HEIGHT: f32 = 34.0;
    const SUMMARY_PADDING_X: f32 = 10.0;
    const TELEMETRY_PADDING_X: f32 = 8.0;
    const TELEMETRY_PADDING_Y: f32 = 3.0;
    const TIME_PADDING_X: f32 = 11.0;
    const TIME_TEXT_SIZE: f32 = 11.0;
    const TIME_WIDTH: f32 = 144.0;
    const TITLE_SIZE: f32 = 15.0;
    const TRANSPORT_HEIGHT: f32 = 28.0;
}

pub(crate) fn header<'a>(
    badge: Option<&'a str>,
    value: Option<&ReadValue<'_>>,
    reads: &dyn Reads,
    palette: RenderPalette,
) -> Element<'a, UiEvent> {
    let Some(badge) = badge else {
        return Space::new().into();
    };
    let analysis = waveform_value(value);
    let title = read_text(reads, "deck.track.title")
        .filter(|title| !title.is_empty())
        .unwrap_or(Consts::NO_TRACK);
    let source = read_text(reads, "deck.track.source_kind").unwrap_or(Consts::NO_SOURCE);
    let remaining = read_scalar(reads, "deck.playback.remaining_secs").unwrap_or(0.0);
    let art = container(
        shaped_text("ART")
            .font(fonts::MONO)
            .size(Consts::ART_LABEL_SIZE)
            .color(palette.muted),
    )
    .center(Consts::ART_SIZE)
    .style(move |_| {
        ContainerStyle::default()
            .background(Background::Color(palette.bg_panel))
            .border(
                Border::default()
                    .width(Consts::ART_BORDER_WIDTH)
                    .color(palette.line),
            )
    });
    let summary = container(
        column![
            shaped_text(title.to_owned())
                .font(fonts::display(Weight::Semibold))
                .size(Consts::TITLE_SIZE)
                .color(palette.text),
            shaped_text(source.to_owned())
                .font(fonts::SANS)
                .size(Consts::ARTIST_SIZE)
                .color(palette.text_dim),
        ]
        .spacing(Consts::READOUT_GAP)
        .width(Length::Fill),
    )
    .height(Length::Fill)
    .align_y(Vertical::Center);
    let badge = container(
        shaped_text(badge)
            .font(fonts::display(Weight::Bold))
            .size(Consts::BADGE_TEXT)
            .color(palette.bg),
    )
    .center(Consts::BADGE_SIZE)
    .style(move |_| ContainerStyle::default().background(Background::Color(palette.accent)));

    let mut children: Vec<Element<'a, UiEvent>> = vec![
        art.into(),
        summary.into(),
        Space::new().width(Length::Fill).into(),
    ];
    if let Some(bpm) = analysis_bpm(analysis) {
        children.push(readout_cell(
            palette,
            "BPM",
            format!("{bpm:.1}"),
            Consts::BPM_CELL_WIDTH,
        ));
    }
    children.push(readout_cell(
        palette,
        "REMAIN",
        format!("-{}", format_time(remaining)),
        Consts::REMAIN_CELL_WIDTH,
    ));
    children.push(badge.into());

    container(
        Row::with_children(children)
            .spacing(Consts::HEADER_GAP)
            .align_y(Alignment::Center)
            .width(Length::Fill),
    )
    .padding([Consts::HEADER_PADDING_Y, Consts::HEADER_PADDING_X])
    .height(Length::Fixed(Consts::HEADER_HEIGHT))
    .width(Length::Fill)
    .align_y(Vertical::Center)
    .style(move |_| ContainerStyle::default().background(Background::Color(palette.bg_deep)))
    .into()
}

pub(crate) fn summary<'a>(
    style: DeckSummaryStyle,
    value: Option<&ReadValue<'_>>,
    reads: &dyn Reads,
    palette: RenderPalette,
) -> Element<'a, UiEvent> {
    let title = match value {
        Some(ReadValue::Text(value)) if !value.is_empty() => (*value).to_owned(),
        _ => read_text(reads, "deck.track.title")
            .filter(|title| !title.is_empty())
            .unwrap_or(Consts::NO_TRACK)
            .to_owned(),
    };
    let source = read_text(reads, "deck.track.source_kind").unwrap_or(Consts::EM_DASH);
    let compact = style == DeckSummaryStyle::Micro;
    let content: Element<'a, UiEvent> = if compact {
        row![
            shaped_text(title)
                .font(fonts::display(Weight::Medium))
                .size(Consts::MICRO_TITLE_SIZE)
                .color(palette.text),
            shaped_text(source.to_owned())
                .font(fonts::SANS)
                .size(Consts::MICRO_SOURCE_SIZE)
                .color(palette.muted),
        ]
        .spacing(Consts::MICRO_SUMMARY_GAP)
        .align_y(Alignment::Center)
        .into()
    } else {
        column![
            shaped_text(title)
                .font(fonts::display(Weight::Semibold))
                .size(Consts::TITLE_SIZE)
                .color(palette.text),
            shaped_text(source.to_owned())
                .font(fonts::SANS)
                .size(Consts::ARTIST_SIZE)
                .color(palette.text_dim),
        ]
        .spacing(Consts::READOUT_GAP)
        .into()
    };

    container(content)
        .height(Length::Fixed(Consts::SUMMARY_HEIGHT))
        .width(Length::FillPortion(Consts::SUMMARY_FILL_PORTION))
        .padding([0.0, Consts::SUMMARY_PADDING_X])
        .align_y(Vertical::Center)
        .style(move |_| ContainerStyle::default().background(Background::Color(palette.bg_panel)))
        .into()
}

pub(crate) fn bpm<'a>(
    placeholder: Option<&str>,
    value: Option<&ReadValue<'_>>,
    reads: &dyn Reads,
    palette: RenderPalette,
) -> Element<'a, UiEvent> {
    let content: Element<'a, UiEvent> = if let Some(bpm) = analysis_bpm(waveform_value(value)) {
        shaped_text(format!("{bpm:.1}"))
            .font(fonts::MONO)
            .size(Consts::BPM_TEXT_SIZE)
            .color(palette.accent_strong)
            .into()
    } else if placeholder == Some("time") {
        let position = read_scalar(reads, "deck.playback.position_secs").unwrap_or(0.0);
        column![
            shaped_text("TIME")
                .font(fonts::MONO)
                .size(Consts::READOUT_LABEL_SIZE)
                .color(palette.muted),
            shaped_text(format_time(position))
                .font(fonts::MONO)
                .size(Consts::BPM_TEXT_SIZE)
                .color(palette.accent_strong),
        ]
        .spacing(Consts::READOUT_GAP)
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
        .style(move |_| ContainerStyle::default().background(Background::Color(palette.bg_panel)))
        .into()
}

pub(crate) fn time<'a>(
    value: Option<&ReadValue<'_>>,
    reads: &dyn Reads,
    palette: RenderPalette,
) -> Element<'a, UiEvent> {
    if !matches!(value, Some(ReadValue::Scalar(_))) {
        return Space::new().into();
    }
    let position = read_scalar(reads, "deck.playback.position_secs").unwrap_or(0.0);
    let duration = read_scalar(reads, "deck.playback.duration_secs").unwrap_or(0.0);
    container(
        shaped_text(format!(
            "{} / {}",
            format_time(position),
            format_time(duration)
        ))
        .font(fonts::MONO)
        .size(Consts::TIME_TEXT_SIZE)
        .color(palette.accent_strong),
    )
    .padding([0.0, Consts::TIME_PADDING_X])
    .center_y(Length::Fill)
    .center_x(Length::Fill)
    .height(Length::Fixed(Consts::TRANSPORT_HEIGHT))
    .width(Length::Fixed(Consts::TIME_WIDTH))
    .style(move |_| ContainerStyle::default().background(Background::Color(palette.bg_deep)))
    .into()
}

pub(crate) fn format_time(seconds: f64) -> String {
    let total = seconds.max(0.0).floor().to_u64().unwrap_or(0);
    let minutes = total / Consts::SECONDS_PER_MINUTE;
    let seconds = total % Consts::SECONDS_PER_MINUTE;
    format!("{minutes}:{seconds:02}")
}

fn readout_cell(
    palette: RenderPalette,
    caption: &'static str,
    value: String,
    width: f32,
) -> Element<'static, UiEvent> {
    container(
        column![
            shaped_text(caption)
                .font(fonts::MONO)
                .size(Consts::READOUT_LABEL_SIZE)
                .color(palette.muted),
            shaped_text(value)
                .font(fonts::MONO)
                .size(Consts::READOUT_VALUE_SIZE)
                .color(palette.accent_strong),
        ]
        .spacing(Consts::READOUT_GAP)
        .align_x(Alignment::End),
    )
    .padding([Consts::TELEMETRY_PADDING_Y, Consts::TELEMETRY_PADDING_X])
    .height(Length::Fixed(Consts::READOUT_HEIGHT))
    .width(Length::Fixed(width))
    .center_y(Length::Fill)
    .style(move |_| {
        ContainerStyle::default()
            .background(Background::Color(palette.bg_inset))
            .border(
                Border::default()
                    .width(chrome::border_width())
                    .color(palette.line),
            )
    })
    .into()
}

fn analysis_bpm(analysis: Option<WaveformView<'_>>) -> Option<f64> {
    analysis
        .and_then(|view| view.bpm)
        .map(f64::from)
        .filter(|bpm| bpm.is_finite() && *bpm > 0.0)
}

fn waveform_value<'data>(value: Option<&ReadValue<'data>>) -> Option<WaveformView<'data>> {
    match value {
        Some(ReadValue::Waveform(analysis)) => Some(*analysis),
        _ => None,
    }
}

fn read_text<'a>(reads: &'a dyn Reads, endpoint: &str) -> Option<&'a str> {
    match reads.get(endpoint) {
        Some(ReadValue::Text(value)) => Some(value),
        _ => None,
    }
}

fn read_scalar(reads: &dyn Reads, endpoint: &str) -> Option<f64> {
    match reads.get(endpoint) {
        Some(ReadValue::Scalar(value)) => Some(value),
        _ => None,
    }
}
