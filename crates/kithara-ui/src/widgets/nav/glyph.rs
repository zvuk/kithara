use iced::{Element, Length, widget::container};

use crate::{
    render::{Icon, Skin, UiEvent},
    widgets::Widget,
};

#[derive(bon::Builder)]
pub(crate) struct Glyph<'skin> {
    icon: Icon,
    skin: &'skin Skin,
}

impl<'a> Widget<'a> for Glyph<'_> {
    fn view(self) -> Element<'a, UiEvent> {
        container(
            self.icon
                .view(self.skin.nav.header_icon_size, self.skin.palette.text),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .into()
    }
}
