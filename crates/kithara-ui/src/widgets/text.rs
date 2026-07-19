use iced::{Element, Length, alignment::Vertical, font::Weight, widget::container};

use crate::{
    module::TextStyle,
    render::{ReadValue, RenderPalette, fonts, shaped_text},
};

struct Consts;

impl Consts {
    const BODY_SIZE: f32 = 13.0;
    const SECTION_SIZE: f32 = 9.0;
    const TRACK_TITLE_SIZE: f32 = 15.0;
}

pub(crate) fn view<'a>(
    style: TextStyle,
    value: Option<&ReadValue<'_>>,
    palette: RenderPalette,
) -> Element<'a, crate::render::UiEvent> {
    let Some(ReadValue::Text(value)) = value else {
        return iced::widget::Space::new().into();
    };
    let (font, size, color) = match style {
        TextStyle::TrackTitle => (
            fonts::display(Weight::Semibold),
            Consts::TRACK_TITLE_SIZE,
            palette.text,
        ),
        TextStyle::Section => (fonts::MONO, Consts::SECTION_SIZE, palette.muted),
        TextStyle::Body => (fonts::SANS, Consts::BODY_SIZE, palette.text),
    };
    container(
        shaped_text((*value).to_owned())
            .font(font)
            .size(size)
            .color(color),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .align_y(Vertical::Center)
    .into()
}
