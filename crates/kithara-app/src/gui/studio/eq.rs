use iced::{
    Alignment, Element, Length,
    font::Weight,
    widget::{column, container, row, text},
};

use super::tokens::{studio_size, studio_type};
use crate::{
    gui::{fonts, view::eq_band_label, widgets},
    theme::gui::GuiPalette,
};

mod consts {
    pub(super) const EQ_MIN_DB: f32 = -24.0;
    pub(super) const EQ_MAX_DB: f32 = 6.0;
    pub(super) const FADER_GAP: f32 = 2.0;
    pub(super) const FADER_LABEL_GAP: f32 = 2.0;
    pub(super) const FADER_EDGE_PAD: f32 = 10.0;
}
use consts::*;

/// Band gain in dB.
#[derive(Debug, Clone, Copy)]
pub(super) struct EqMsg(pub(super) usize, pub(super) f32);

pub(super) fn view_eq(bands: &[f32], p: GuiPalette) -> Element<'static, EqMsg> {
    let total = bands.len();
    let strip = bands.iter().copied().enumerate().fold(
        row![]
            .spacing(FADER_GAP)
            .align_y(Alignment::End)
            .width(Length::Fill),
        |strip, (index, value)| strip.push(fader_cell(index, total, value, p)),
    );

    container(strip)
        .width(Length::Fill)
        .padding([FADER_EDGE_PAD, 0.0])
        .into()
}

fn fader_cell(index: usize, total: usize, value: f32, p: GuiPalette) -> Element<'static, EqMsg> {
    let value = value.clamp(EQ_MIN_DB, EQ_MAX_DB);

    container(
        column![
            text(format!("{value:+.0}"))
                .size(studio_type::MONO_SM)
                .line_height(1.0)
                .font(fonts::mono(Weight::Medium))
                .color(p.text_dim),
            widgets::vfader(
                widgets::VFaderParams {
                    value,
                    min: EQ_MIN_DB,
                    max: EQ_MAX_DB,
                    height: studio_size::FADER_HEIGHT,
                },
                p,
                move |gain| EqMsg(index, gain),
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
