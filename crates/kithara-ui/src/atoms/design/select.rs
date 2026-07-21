use iced::{
    Background, Element, Length,
    widget::{Row, Space, container, container::Style as ContainerStyle},
};

use crate::{
    render::{Skin, UiEvent, fonts, shaped_text},
    widgets::Widget,
};

#[derive(bon::Builder)]
pub(crate) struct Select<'a, 'skin> {
    label: &'a str,
    skin: &'skin Skin,
}

impl<'a> Widget<'a> for Select<'a, '_> {
    fn view(self) -> Element<'a, UiEvent> {
        let metrics = self.skin.select;
        let content = Row::with_children([
            shaped_text(self.label)
                .font(fonts::mono(metrics.text.weight))
                .size(metrics.text.size)
                .color(self.skin.color(metrics.text_color))
                .into(),
            Space::new().width(Length::Fill).into(),
            shaped_text("\u{2304}")
                .font(fonts::mono(metrics.text.weight))
                .size(metrics.chevron_size)
                .color(self.skin.color(metrics.chevron_color))
                .into(),
        ]);
        let border = self.skin.border(metrics.frame);
        let background = self.skin.color(metrics.background);
        container(content)
            .padding([metrics.padding_y, metrics.padding_x])
            .width(Length::Fill)
            .height(Length::Fill)
            .center_y(Length::Fill)
            .style(move |_| {
                ContainerStyle::default()
                    .background(Background::Color(background))
                    .border(border)
            })
            .into()
    }
}
