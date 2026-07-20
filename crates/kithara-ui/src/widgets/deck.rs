use iced::{
    Alignment, Background, Element, Length,
    alignment::Vertical,
    widget::{Space, column, container, container::Style as ContainerStyle, row},
};
use num_traits::ToPrimitive;

use crate::{
    module::DeckSummaryStyle,
    render::{ReadValue, Reads, Skin, UiEvent, WaveformView, fonts, shaped_text},
    widgets::Widget,
};

const fn em_dash() -> &'static str {
    "\u{2014}"
}

const fn no_track() -> &'static str {
    "No track loaded"
}

const fn seconds_per_minute() -> u64 {
    60
}

#[derive(bon::Builder)]
pub(crate) struct DeckSummary<'value, 'data, 'reads, 'skin> {
    style: DeckSummaryStyle,
    value: Option<&'value ReadValue<'data>>,
    reads: &'reads dyn Reads,
    skin: &'skin Skin,
}

impl<'a> Widget<'a> for DeckSummary<'_, '_, '_, '_> {
    fn view(self) -> Element<'a, UiEvent> {
        let palette = self.skin.palette;
        let title = match self.value {
            Some(ReadValue::Text(value)) if !value.is_empty() => (*value).to_owned(),
            _ => read_text(self.reads, "deck.track.title")
                .filter(|title| !title.is_empty())
                .unwrap_or(no_track())
                .to_owned(),
        };
        let source = read_text(self.reads, "deck.track.source_kind").unwrap_or(em_dash());
        let compact = self.style == DeckSummaryStyle::Micro;
        let content: Element<'a, UiEvent> = if compact {
            row![
                shaped_text(title)
                    .font(fonts::display(self.skin.deck.micro_title.weight))
                    .size(self.skin.deck.micro_title.size)
                    .color(palette.text),
                shaped_text(source.to_owned())
                    .font(fonts::sans(self.skin.deck.micro_source.weight))
                    .size(self.skin.deck.micro_source.size)
                    .color(palette.muted),
            ]
            .spacing(self.skin.deck.micro_summary_gap)
            .align_y(Alignment::Center)
            .into()
        } else {
            column![
                shaped_text(title)
                    .font(fonts::display(self.skin.deck.title.weight))
                    .size(self.skin.deck.title.size)
                    .color(palette.text),
                shaped_text(source.to_owned())
                    .font(fonts::sans(self.skin.deck.artist.weight))
                    .size(self.skin.deck.artist.size)
                    .color(palette.text_dim),
            ]
            .spacing(self.skin.deck.readout_gap)
            .into()
        };

        container(content)
            .height(Length::Fixed(self.skin.deck.summary_height))
            .width(Length::FillPortion(self.skin.deck.summary_fill))
            .padding([
                self.skin.deck.summary_padding_y,
                self.skin.deck.summary_padding_x,
            ])
            .align_y(Vertical::Center)
            .clip(true)
            .style(move |_| {
                ContainerStyle::default().background(Background::Color(palette.bg_panel))
            })
            .into()
    }
}

#[derive(bon::Builder)]
pub(crate) struct Bpm<'placeholder, 'value, 'data, 'reads, 'skin> {
    placeholder: Option<&'placeholder str>,
    value: Option<&'value ReadValue<'data>>,
    reads: &'reads dyn Reads,
    skin: &'skin Skin,
}

impl<'a> Widget<'a> for Bpm<'_, '_, '_, '_, '_> {
    fn view(self) -> Element<'a, UiEvent> {
        let palette = self.skin.palette;
        let content: Element<'a, UiEvent> = if let Some(bpm) =
            analysis_bpm(waveform_value(self.value))
        {
            shaped_text(format!("{bpm:.1}"))
                .font(fonts::mono(self.skin.deck.bpm_text.weight))
                .size(self.skin.deck.bpm_text.size)
                .color(palette.accent_strong)
                .into()
        } else if self.placeholder == Some("time") {
            let position = read_scalar(self.reads, "deck.playback.position_secs").unwrap_or(0.0);
            column![
                shaped_text("TIME")
                    .font(fonts::mono(self.skin.deck.readout_label.weight))
                    .size(self.skin.deck.readout_label.size)
                    .color(palette.muted),
                shaped_text(format_time(position))
                    .font(fonts::mono(self.skin.deck.bpm_text.weight))
                    .size(self.skin.deck.bpm_text.size)
                    .color(palette.accent_strong),
            ]
            .spacing(self.skin.deck.readout_gap)
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
            .style(move |_| {
                ContainerStyle::default().background(Background::Color(palette.bg_panel))
            })
            .into()
    }
}

#[derive(bon::Builder)]
pub(crate) struct Time<'value, 'data, 'reads, 'skin> {
    value: Option<&'value ReadValue<'data>>,
    reads: &'reads dyn Reads,
    skin: &'skin Skin,
}

impl<'a> Widget<'a> for Time<'_, '_, '_, '_> {
    fn view(self) -> Element<'a, UiEvent> {
        if !matches!(self.value, Some(ReadValue::Scalar(_))) {
            return Space::new().into();
        }
        let palette = self.skin.palette;
        let position = read_scalar(self.reads, "deck.playback.position_secs").unwrap_or(0.0);
        let duration = read_scalar(self.reads, "deck.playback.duration_secs").unwrap_or(0.0);
        container(
            shaped_text(format!(
                "{} / {}",
                format_time(position),
                format_time(duration)
            ))
            .font(fonts::mono(self.skin.deck.time_text.weight))
            .size(self.skin.deck.time_text.size)
            .color(palette.accent_strong),
        )
        .padding([self.skin.deck.time_padding_y, self.skin.deck.time_padding_x])
        .center_y(Length::Fill)
        .center_x(Length::Fill)
        .height(Length::Fixed(self.skin.deck.transport_height))
        .width(Length::Fixed(self.skin.deck.time_size.w.min()))
        .style(move |_| ContainerStyle::default().background(Background::Color(palette.bg_deep)))
        .into()
    }
}

pub(crate) fn format_time(seconds: f64) -> String {
    let total = seconds.max(0.0).floor().to_u64().unwrap_or(0);
    let minutes = total / seconds_per_minute();
    let seconds = total % seconds_per_minute();
    format!("{minutes}:{seconds:02}")
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
