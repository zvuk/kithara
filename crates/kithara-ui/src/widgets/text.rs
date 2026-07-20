use iced::{Element, Length, alignment::Vertical, widget::container};

use crate::{
    module::TextStyle,
    render::{ReadValue, Skin, UiEvent, fonts, shaped_text},
    widgets::Widget,
};

#[derive(bon::Builder)]
pub(crate) struct Text<'value, 'data, 'skin> {
    style: TextStyle,
    value: Option<&'value ReadValue<'data>>,
    skin: &'skin Skin,
}

impl<'a> Widget<'a> for Text<'_, '_, '_> {
    fn view(self) -> Element<'a, UiEvent> {
        let Some(ReadValue::Text(value)) = self.value else {
            return iced::widget::Space::new().into();
        };
        let palette = self.skin.palette;
        let (font, size, color) = match self.style {
            TextStyle::TrackTitle => (
                fonts::display(self.skin.text.track_title.weight),
                self.skin.text.track_title.size,
                palette.text,
            ),
            TextStyle::Section => (
                fonts::mono(self.skin.text.section.weight),
                self.skin.text.section.size,
                palette.muted,
            ),
            TextStyle::Body => (
                fonts::sans(self.skin.text.body.weight),
                self.skin.text.body.size,
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
}
