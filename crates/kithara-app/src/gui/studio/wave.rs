use iced::{
    Alignment, Background, Border, Color, Element, Length, Padding, Theme,
    alignment::{Horizontal, Vertical},
    widget::{
        Space, button,
        button::{Status as ButtonStatus, Style as ButtonStyle},
        column, container,
        container::Style as ContainerStyle,
        row, stack, text,
    },
};
use kithara::audio::Waveform;
use kithara_platform::sync::Arc;

use super::{
    module::Module,
    tokens::{studio_size, studio_space, studio_type},
};
use crate::{
    gui::{
        fonts,
        tokens::gap,
        widgets::{self, BeatMarks, Viewport, WaveEvent, WaveMsg},
    },
    theme::gui::GuiPalette,
};

/// The waveform cluster: the canvas with its zoom control and the
/// elapsed / total readout.
pub(super) struct WaveProps<'a> {
    pub(super) wave: Option<&'a Waveform>,
    pub(super) beats: Arc<[f32]>,
    pub(super) downbeats: Arc<[f32]>,
    pub(super) view: Viewport,
    pub(super) elapsed: String,
    pub(super) total: String,
    pub(super) progress: f32,
    pub(super) duration: f64,
}

pub(super) fn view_wave<'a>(props: WaveProps<'a>, p: GuiPalette) -> Element<'a, WaveEvent> {
    let canvas: Element<'a, WaveEvent> = props.wave.map_or_else(
        || {
            Space::new()
                .width(Length::Fill)
                .height(Length::Fixed(studio_size::WAVEFORM_HEIGHT))
                .into()
        },
        |wave| {
            widgets::waveform(
                wave.clone(),
                BeatMarks {
                    beats: Arc::clone(&props.beats),
                    downbeats: Arc::clone(&props.downbeats),
                },
                props.progress,
                props.duration,
                props.view,
                studio_size::WAVEFORM_HEIGHT,
                p,
            )
        },
    );

    let wave_box = container(canvas)
        .width(Length::Fill)
        .height(Length::Fixed(studio_size::WAVEFORM_HEIGHT))
        .style(waveform_style(p));

    let wave_layer: Element<'a, WaveEvent> = if props.wave.is_some() {
        stack![wave_box, zoom_control(props.view.zoom(), p)].into()
    } else {
        wave_box.into()
    };

    Module::new().pad(studio_space::CLUSTER).wrap(
        column![
            wave_layer,
            row![
                time_label(props.elapsed, p),
                Space::new().width(Length::Fill),
                time_label(props.total, p),
            ]
            .align_y(Alignment::Center),
        ]
        .spacing(gap::INLINE_TIGHT),
    )
}

fn time_label(label: String, p: GuiPalette) -> Element<'static, WaveEvent> {
    text(label)
        .size(studio_type::MONO_SM)
        .font(fonts::MONO)
        .color(p.muted)
        .into()
}

fn zoom_control(zoom: f32, p: GuiPalette) -> Element<'static, WaveEvent> {
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

fn zoom_button(label: &'static str, msg: WaveMsg, p: GuiPalette) -> Element<'static, WaveEvent> {
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
    .on_press(WaveEvent::View(msg))
    .into()
}

fn zoom_pill_style(p: GuiPalette) -> impl Fn(&Theme) -> ContainerStyle {
    move |_theme| {
        ContainerStyle::default()
            .background(Background::Color(p.bg_panel))
            .border(Border::default().width(1.0).color(p.line))
    }
}

fn zoom_button_style(p: GuiPalette, status: ButtonStatus) -> ButtonStyle {
    let background = match status {
        ButtonStatus::Hovered | ButtonStatus::Pressed => Some(Background::Color(p.bg_elev)),
        ButtonStatus::Active | ButtonStatus::Disabled => {
            Some(Background::Color(Color::TRANSPARENT))
        }
    };

    ButtonStyle {
        background,
        text_color: p.text,
        ..ButtonStyle::default()
    }
}

fn waveform_style(p: GuiPalette) -> impl Fn(&Theme) -> ContainerStyle {
    move |_theme| {
        ContainerStyle::default()
            .background(Background::Color(p.bg_inset))
            .border(Border::default().width(1.0).color(p.line_soft))
    }
}
