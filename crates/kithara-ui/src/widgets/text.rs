use iced::{Element, Length, alignment::Vertical, font::Weight, widget::container};

use crate::{
    registry::{ControlKindDesc, PropKind, ValueKind},
    render::{ReadValue, RenderPalette, fonts, shaped_text},
    size::{Dim, SizeSpec},
};

struct Consts;

impl Consts {
    const BODY_SIZE: f32 = 13.0;
    const HEIGHT: f32 = 18.0;
    const TRACK_TITLE_SIZE: f32 = 15.0;
}

pub(crate) fn desc() -> ControlKindDesc {
    ControlKindDesc::new(Some(ValueKind::Text), None)
        .with_prop("style", PropKind::Text)
        .with_size(SizeSpec::new(Dim::Fill, Dim::Fixed(Consts::HEIGHT)))
}

pub(crate) fn view<'a>(
    style: Option<&str>,
    value: Option<&ReadValue<'_>>,
    palette: RenderPalette,
) -> Element<'a, crate::render::UiEvent> {
    let Some(ReadValue::Text(value)) = value else {
        return iced::widget::Space::new().into();
    };
    let (font, size) = if style == Some("track-title") {
        (fonts::display(Weight::Semibold), Consts::TRACK_TITLE_SIZE)
    } else {
        (fonts::SANS, Consts::BODY_SIZE)
    };
    container(
        shaped_text((*value).to_owned())
            .font(font)
            .size(size)
            .color(palette.text),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .align_y(Vertical::Center)
    .into()
}
