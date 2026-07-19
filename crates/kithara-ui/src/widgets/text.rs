use iced::{Element, Length, alignment::Vertical, widget::container};

use crate::{
    module::TextStyle,
    render::{ReadValue, Skin, fonts, shaped_text},
};

pub(crate) fn view<'a>(
    style: TextStyle,
    value: Option<&ReadValue<'_>>,
    skin: &Skin,
) -> Element<'a, crate::render::UiEvent> {
    let Some(ReadValue::Text(value)) = value else {
        return iced::widget::Space::new().into();
    };
    let palette = skin.palette;
    let (font, size, color) = match style {
        TextStyle::TrackTitle => (
            fonts::display(skin.text.track_title.weight),
            skin.text.track_title.size,
            palette.text,
        ),
        TextStyle::Section => (
            fonts::mono(skin.text.section.weight),
            skin.text.section.size,
            palette.muted,
        ),
        TextStyle::Body => (
            fonts::sans(skin.text.body.weight),
            skin.text.body.size,
            palette.text,
        ),
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
