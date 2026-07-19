use iced::{
    Alignment, Background, Border, Color, Element, Length, Theme,
    font::Weight,
    widget::{Space, button, button::Style as ButtonStyle, column, container, row, slider, text},
};

use super::{
    module::Module,
    tokens::{studio_size, studio_space, studio_type},
};
use crate::{
    gui::{fonts, tokens::gap, widgets},
    theme::gui::GuiPalette,
};

mod consts {
    pub(super) const TRIM_HEIGHT: f32 = 96.0;
    pub(super) const MUTE_HEIGHT: f32 = 20.0;
    pub(super) const LABEL_BOX: f32 = 18.0;
}
use consts::*;

/// One channel strip: what this deck contributes to the session mix.
#[derive(Debug, Clone, Copy)]
pub(super) struct StripProps {
    /// Channel letter (A, B, …) assigned by the composer.
    pub(super) label: char,
    pub(super) trim: f32,
    pub(super) muted: bool,
    pub(super) focused: bool,
}

#[derive(Debug, Clone, Copy)]
pub(super) enum StripMsg {
    Trim(f32),
    Mute(bool),
    Focus,
}

/// The crossfader lane under the strips.
#[derive(Debug, Clone, Copy)]
pub(super) struct CrossProps {
    pub(super) position: f32,
    pub(super) master: f32,
}

#[derive(Debug, Clone, Copy)]
pub(super) enum CrossMsg {
    Position(f32),
    Master(f32),
}

pub(super) fn view_strip(props: StripProps, p: GuiPalette) -> Element<'static, StripMsg> {
    let StripProps {
        label,
        trim,
        muted,
        focused,
    } = props;

    let fader = widgets::vfader(
        widgets::VFaderParams {
            value: trim,
            min: 0.0,
            max: 1.0,
            height: TRIM_HEIGHT,
        },
        p,
        StripMsg::Trim,
    );

    let strip = column![
        text(format!("{trim:.2}"))
            .size(studio_type::MONO_XS)
            .font(fonts::mono(Weight::Medium))
            .color(p.text_dim),
        fader,
        mute_button(muted, p),
        channel_label(label, focused, p),
    ]
    .align_x(Alignment::Center)
    .spacing(gap::INLINE_TIGHT);

    button(container(strip).center_x(Length::Fill))
        .padding(studio_space::CLUSTER)
        .style(move |_theme: &Theme, _status| strip_style(p, focused))
        .on_press(StripMsg::Focus)
        .into()
}

pub(super) fn view_crossfader(props: CrossProps, p: GuiPalette) -> Element<'static, CrossMsg> {
    let CrossProps { position, master } = props;

    let fade = slider(0.0..=1.0, position, CrossMsg::Position)
        .step(0.01_f32)
        .width(Length::Fill);

    let master_row = row![
        caption("MASTER", p),
        slider(0.0..=1.0, master, CrossMsg::Master)
            .step(0.01_f32)
            .width(Length::Fill),
        value_text(format!("{master:.2}"), p),
    ]
    .align_y(Alignment::Center)
    .spacing(gap::INLINE);

    Module::new()
        .bg(p.bg_panel)
        .pad(studio_space::CLUSTER)
        .wrap(
            column![
                fade,
                row![
                    caption("A", p),
                    Space::new().width(Length::Fill),
                    caption("XFADE", p),
                    Space::new().width(Length::Fill),
                    caption("B", p),
                ]
                .align_y(Alignment::Center),
                master_row,
            ]
            .spacing(gap::INLINE_TIGHT),
        )
}

fn mute_button(muted: bool, p: GuiPalette) -> Element<'static, StripMsg> {
    // Active toggle: gold fill with on-gold text, per the design system.
    let color = if muted { p.bg } else { p.muted };
    let background = if muted { p.danger } else { p.bg_inset };

    button(
        container(
            text("MUTE")
                .size(studio_type::MONO_XS)
                .font(fonts::mono(Weight::Semibold))
                .color(color),
        )
        .center_x(Length::Fill)
        .center_y(Length::Fill),
    )
    .width(Length::Fill)
    .height(Length::Fixed(MUTE_HEIGHT))
    .padding(0)
    .style(move |_theme: &Theme, _status| ButtonStyle {
        background: Some(Background::Color(background)),
        text_color: color,
        border: Border::default().width(1.0).color(p.line),
        ..ButtonStyle::default()
    })
    .on_press(StripMsg::Mute(!muted))
    .into()
}

fn channel_label(label: char, focused: bool, p: GuiPalette) -> Element<'static, StripMsg> {
    container(
        text(label.to_string())
            .size(studio_type::BODY_MD)
            .font(fonts::display(Weight::Bold))
            .color(if focused { p.accent } else { p.text_dim }),
    )
    .center_x(Length::Fixed(LABEL_BOX))
    .center_y(Length::Fixed(LABEL_BOX))
    .into()
}

fn caption<M: 'static>(label: &str, p: GuiPalette) -> Element<'static, M> {
    text(label.to_string())
        .size(studio_type::MONO_XS)
        .font(fonts::mono(Weight::Medium))
        .color(p.muted)
        .into()
}

fn value_text<M: 'static>(label: String, p: GuiPalette) -> Element<'static, M> {
    text(label)
        .size(studio_type::MONO_XS)
        .font(fonts::mono(Weight::Medium))
        .color(p.text_dim)
        .into()
}

/// The focused strip is outlined; an unfocused one stays flat.
fn strip_style(p: GuiPalette, focused: bool) -> ButtonStyle {
    ButtonStyle {
        background: Some(Background::Color(if focused {
            p.bg_elev
        } else {
            Color::TRANSPARENT
        })),
        text_color: p.text,
        border: Border::default()
            .width(studio_size::DIVIDER)
            .color(if focused {
                p.accent
            } else {
                Color::TRANSPARENT
            }),
        ..ButtonStyle::default()
    }
}
