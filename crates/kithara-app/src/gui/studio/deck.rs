use iced::{
    Alignment, Element, Length, Padding, Theme,
    alignment::{Horizontal, Vertical},
    font::Weight,
    widget::{column, container, container::Style as ContainerStyle, row, stack, text},
};
use kithara::audio::Waveform;
use kithara_platform::sync::Arc;
use num_traits::cast::AsPrimitive;

use super::{
    eq::{self, EqMsg},
    styles::{horizontal_divider, vertical_divider},
    timestretch::{self, TsProps},
    tokens::{studio_size, studio_space, studio_type},
    transport::{self, TransportMsg},
    wave::{self, WaveProps},
};
use crate::{
    gui::{
        deck::DeckMsg,
        fonts,
        tokens::gap,
        widgets::{Viewport, WaveEvent},
    },
    theme::gui::GuiPalette,
};

/// Everything one deck renders. Assembled by the composer from a deck
/// snapshot; the preset below knows nothing about which deck it is.
pub(crate) struct DeckProps<'a> {
    pub(crate) title: &'a str,
    pub(crate) subtitle: String,
    pub(crate) elapsed: String,
    pub(crate) total: String,
    pub(crate) wave: Option<&'a Waveform>,
    pub(crate) beats: Arc<[f32]>,
    pub(crate) downbeats: Arc<[f32]>,
    pub(crate) view: Viewport,
    pub(crate) eq_bands: &'a [f32],
    pub(crate) timestretch: TsProps,
    pub(crate) progress: f32,
    pub(crate) duration: f64,
    pub(crate) playing: bool,
}

/// The deck as it stands today: waveform cluster over transport + EQ over the
/// timestretch panel. A different arrangement is a different composer, not a
/// different block.
pub(crate) fn view_deck(props: DeckProps<'_>, p: GuiPalette) -> Element<'_, DeckMsg> {
    let DeckProps {
        title,
        subtitle,
        elapsed,
        total,
        wave,
        beats,
        downbeats,
        view,
        eq_bands,
        timestretch: ts,
        progress,
        duration,
        playing,
    } = props;

    let wave_block = wave::view_wave(
        WaveProps {
            wave,
            beats,
            downbeats,
            view,
            elapsed,
            total,
            progress,
            duration,
        },
        p,
    )
    .map(wave_event);

    container(column![
        stack![wave_block, header(title, subtitle, p)],
        horizontal_divider(studio_size::DIVIDER, p.line_soft),
        transport_and_eq(playing, eq_bands, p),
        horizontal_divider(studio_size::DIVIDER, p.line_soft),
        timestretch::view_timestretch(ts, p).map(ts_msg),
    ])
    .width(Length::Fill)
    .height(Length::Fill)
    .style(deck_style(p))
    .into()
}

/// Track identity rides over the waveform, pinned to its upper-left.
fn header<'a>(title: &'a str, subtitle: String, p: GuiPalette) -> Element<'a, DeckMsg> {
    let lines = column![
        text(title)
            .size(studio_type::TRACK)
            .font(fonts::display(Weight::Medium))
            .color(p.text),
        text(subtitle)
            .size(studio_type::BODY_SM)
            .font(fonts::SANS)
            .color(p.text_dim),
    ]
    .spacing(gap::TEXT_STACK);

    container(lines)
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(Horizontal::Left)
        .align_y(Vertical::Top)
        .padding(Padding::from(8.0))
        .into()
}

/// Playhead fraction in `[0, 1]`. Zero duration reads as 0.
pub(crate) fn playhead_progress(position: f64, duration: f64) -> f32 {
    if duration <= 0.0 {
        return 0.0;
    }
    let progress: f32 = (position / duration).clamp(0.0, 1.0).as_();
    progress
}

fn transport_and_eq<'a>(playing: bool, eq_bands: &[f32], p: GuiPalette) -> Element<'a, DeckMsg> {
    super::module::Module::new()
        .bg(p.bg_panel)
        .pad(studio_space::CLUSTER)
        .wrap(
            row![
                transport::view_transport(playing, p).map(transport_msg),
                vertical_divider(studio_size::DIVIDER, studio_size::FADER_HEIGHT, p.line_soft),
                eq::view_eq(eq_bands, p).map(eq_msg),
            ]
            .align_y(Alignment::Center)
            .spacing(gap::CONTENT),
        )
}

fn wave_event(event: WaveEvent) -> DeckMsg {
    match event {
        WaveEvent::View(msg) => DeckMsg::Wave(msg),
        WaveEvent::Seek(secs) => DeckMsg::SeekTo(secs),
    }
}

fn transport_msg(msg: TransportMsg) -> DeckMsg {
    match msg {
        TransportMsg::PlayPause => DeckMsg::TogglePlayPause,
        TransportMsg::Next => DeckMsg::Next,
        TransportMsg::Prev => DeckMsg::Prev,
    }
}

fn eq_msg(EqMsg(band, gain): EqMsg) -> DeckMsg {
    DeckMsg::EqBandChanged(band, gain)
}

fn ts_msg(msg: timestretch::TsMsg) -> DeckMsg {
    match msg {
        timestretch::TsMsg::SetTempo(t) => DeckMsg::SetTempo(t),
        timestretch::TsMsg::SetRange(r) => DeckMsg::SetRange(r),
        timestretch::TsMsg::Nudge(d) => DeckMsg::Nudge(d),
        timestretch::TsMsg::ResetTempo => DeckMsg::ResetTempo,
        #[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
        timestretch::TsMsg::ToggleKeyLock => DeckMsg::ToggleKeyLock,
        #[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
        timestretch::TsMsg::SelectBackend(k) => DeckMsg::SelectBackend(k),
    }
}

fn deck_style(p: GuiPalette) -> impl Fn(&Theme) -> ContainerStyle {
    move |_theme| ContainerStyle::default().color(p.text)
}
