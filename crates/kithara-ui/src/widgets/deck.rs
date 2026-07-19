use iced::{
    Alignment, Background, Element, Length,
    alignment::Vertical,
    widget::{Row, Space, column, container, container::Style as ContainerStyle, row},
};
use num_traits::ToPrimitive;

use crate::{
    module::DeckSummaryStyle,
    render::{ReadValue, Reads, Skin, UiEvent, WaveformView, fonts, shaped_text},
};

const fn em_dash() -> &'static str {
    "\u{2014}"
}

const fn no_source() -> &'static str {
    "no source"
}

const fn no_track() -> &'static str {
    "No track loaded"
}

const fn seconds_per_minute() -> u64 {
    60
}

pub(crate) fn header<'a>(
    badge: Option<&'a str>,
    value: Option<&ReadValue<'_>>,
    reads: &dyn Reads,
    skin: &Skin,
) -> Element<'a, UiEvent> {
    let Some(badge) = badge else {
        return Space::new().into();
    };
    let analysis = waveform_value(value);
    let title = read_text(reads, "deck.track.title")
        .filter(|title| !title.is_empty())
        .unwrap_or(no_track());
    let source = read_text(reads, "deck.track.source_kind").unwrap_or(no_source());
    let remaining = read_scalar(reads, "deck.playback.remaining_secs").unwrap_or(0.0);
    let palette = skin.palette;
    let art_border = skin.border(skin.deck.art_frame);
    let art = container(
        shaped_text("ART")
            .font(fonts::mono(skin.deck.art_label.weight))
            .size(skin.deck.art_label.size)
            .color(palette.muted),
    )
    .center(skin.deck.art_size)
    .style(move |_| {
        ContainerStyle::default()
            .background(Background::Color(palette.bg_panel))
            .border(art_border)
    });
    let summary = container(
        column![
            shaped_text(title.to_owned())
                .font(fonts::display(skin.deck.title.weight))
                .size(skin.deck.title.size)
                .color(palette.text),
            shaped_text(source.to_owned())
                .font(fonts::sans(skin.deck.artist.weight))
                .size(skin.deck.artist.size)
                .color(palette.text_dim),
        ]
        .spacing(skin.deck.readout_gap)
        .width(Length::Fill),
    )
    .height(Length::Fill)
    .align_y(Vertical::Center);
    let badge = container(
        shaped_text(badge)
            .font(fonts::display(skin.deck.badge_text.weight))
            .size(skin.deck.badge_text.size)
            .color(palette.bg),
    )
    .center(skin.deck.badge_size)
    .style({
        let badge_border = skin.border(skin.deck.badge_frame);
        move |_| {
            ContainerStyle::default()
                .background(Background::Color(palette.accent))
                .border(badge_border)
        }
    });

    let mut children: Vec<Element<'a, UiEvent>> = vec![
        art.into(),
        summary.into(),
        Space::new().width(Length::Fill).into(),
    ];
    if let Some(bpm) = analysis_bpm(analysis) {
        children.push(readout_cell(
            skin,
            "BPM",
            format!("{bpm:.1}"),
            skin.deck.bpm_cell_width,
        ));
    }
    children.push(readout_cell(
        skin,
        "REMAIN",
        format!("-{}", format_time(remaining)),
        skin.deck.remain_cell_width,
    ));
    children.push(badge.into());

    container(
        Row::with_children(children)
            .spacing(skin.deck.header_gap)
            .align_y(Alignment::Center)
            .width(Length::Fill),
    )
    .padding([skin.deck.header_padding_y, skin.deck.header_padding_x])
    .height(Length::Fixed(skin.deck.header_height))
    .width(Length::Fill)
    .align_y(Vertical::Center)
    .style(move |_| ContainerStyle::default().background(Background::Color(palette.bg_deep)))
    .into()
}

pub(crate) fn summary<'a>(
    style: DeckSummaryStyle,
    value: Option<&ReadValue<'_>>,
    reads: &dyn Reads,
    skin: &Skin,
) -> Element<'a, UiEvent> {
    let palette = skin.palette;
    let title = match value {
        Some(ReadValue::Text(value)) if !value.is_empty() => (*value).to_owned(),
        _ => read_text(reads, "deck.track.title")
            .filter(|title| !title.is_empty())
            .unwrap_or(no_track())
            .to_owned(),
    };
    let source = read_text(reads, "deck.track.source_kind").unwrap_or(em_dash());
    let compact = style == DeckSummaryStyle::Micro;
    let content: Element<'a, UiEvent> = if compact {
        row![
            shaped_text(title)
                .font(fonts::display(skin.deck.micro_title.weight))
                .size(skin.deck.micro_title.size)
                .color(palette.text),
            shaped_text(source.to_owned())
                .font(fonts::sans(skin.deck.micro_source.weight))
                .size(skin.deck.micro_source.size)
                .color(palette.muted),
        ]
        .spacing(skin.deck.micro_summary_gap)
        .align_y(Alignment::Center)
        .into()
    } else {
        column![
            shaped_text(title)
                .font(fonts::display(skin.deck.title.weight))
                .size(skin.deck.title.size)
                .color(palette.text),
            shaped_text(source.to_owned())
                .font(fonts::sans(skin.deck.artist.weight))
                .size(skin.deck.artist.size)
                .color(palette.text_dim),
        ]
        .spacing(skin.deck.readout_gap)
        .into()
    };

    container(content)
        .height(Length::Fixed(skin.deck.summary_height))
        .width(Length::FillPortion(skin.deck.summary_fill))
        .padding([skin.deck.summary_padding_y, skin.deck.summary_padding_x])
        .align_y(Vertical::Center)
        .style(move |_| ContainerStyle::default().background(Background::Color(palette.bg_panel)))
        .into()
}

pub(crate) fn bpm<'a>(
    placeholder: Option<&str>,
    value: Option<&ReadValue<'_>>,
    reads: &dyn Reads,
    skin: &Skin,
) -> Element<'a, UiEvent> {
    let palette = skin.palette;
    let content: Element<'a, UiEvent> = if let Some(bpm) = analysis_bpm(waveform_value(value)) {
        shaped_text(format!("{bpm:.1}"))
            .font(fonts::mono(skin.deck.bpm_text.weight))
            .size(skin.deck.bpm_text.size)
            .color(palette.accent_strong)
            .into()
    } else if placeholder == Some("time") {
        let position = read_scalar(reads, "deck.playback.position_secs").unwrap_or(0.0);
        column![
            shaped_text("TIME")
                .font(fonts::mono(skin.deck.readout_label.weight))
                .size(skin.deck.readout_label.size)
                .color(palette.muted),
            shaped_text(format_time(position))
                .font(fonts::mono(skin.deck.bpm_text.weight))
                .size(skin.deck.bpm_text.size)
                .color(palette.accent_strong),
        ]
        .spacing(skin.deck.readout_gap)
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
    skin: &Skin,
) -> Element<'a, UiEvent> {
    if !matches!(value, Some(ReadValue::Scalar(_))) {
        return Space::new().into();
    }
    let palette = skin.palette;
    let position = read_scalar(reads, "deck.playback.position_secs").unwrap_or(0.0);
    let duration = read_scalar(reads, "deck.playback.duration_secs").unwrap_or(0.0);
    container(
        shaped_text(format!(
            "{} / {}",
            format_time(position),
            format_time(duration)
        ))
        .font(fonts::mono(skin.deck.time_text.weight))
        .size(skin.deck.time_text.size)
        .color(palette.accent_strong),
    )
    .padding([skin.deck.time_padding_y, skin.deck.time_padding_x])
    .center_y(Length::Fill)
    .center_x(Length::Fill)
    .height(Length::Fixed(skin.deck.transport_height))
    .width(Length::Fixed(skin.deck.time_size.w.min()))
    .style(move |_| ContainerStyle::default().background(Background::Color(palette.bg_deep)))
    .into()
}

pub(crate) fn format_time(seconds: f64) -> String {
    let total = seconds.max(0.0).floor().to_u64().unwrap_or(0);
    let minutes = total / seconds_per_minute();
    let seconds = total % seconds_per_minute();
    format!("{minutes}:{seconds:02}")
}

fn readout_cell(
    skin: &Skin,
    caption: &'static str,
    value: String,
    width: f32,
) -> Element<'static, UiEvent> {
    let palette = skin.palette;
    container(
        column![
            shaped_text(caption)
                .font(fonts::mono(skin.deck.readout_label.weight))
                .size(skin.deck.readout_label.size)
                .color(palette.muted),
            shaped_text(value)
                .font(fonts::mono(skin.deck.readout_value.weight))
                .size(skin.deck.readout_value.size)
                .color(palette.accent_strong),
        ]
        .spacing(skin.deck.readout_gap)
        .align_x(Alignment::End),
    )
    .padding([skin.deck.telemetry_padding_y, skin.deck.telemetry_padding_x])
    .height(Length::Fixed(skin.deck.readout_height))
    .width(Length::Fixed(width))
    .center_y(Length::Fill)
    .style({
        let border = skin.border(skin.deck.readout_frame);
        move |_| {
            ContainerStyle::default()
                .background(Background::Color(palette.bg_inset))
                .border(border)
        }
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
