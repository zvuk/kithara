use iced::{
    Background, Element, Length, Theme,
    alignment::Vertical,
    widget::{button, button::Style as ButtonStyle, container},
};

use crate::{
    render::{Skin, UiEvent, WindowCommand, typography::styled_text},
    widgets::Widget,
};

#[derive(bon::Builder)]
pub(crate) struct TitleBar<'label, 'skin> {
    label: &'label str,
    skin: &'skin Skin,
}

impl<'a> Widget<'a> for TitleBar<'_, '_> {
    fn view(self) -> Element<'a, UiEvent> {
        let metrics = self.skin.window;
        let background = self.skin.color(metrics.titlebar_background);
        let content = container(styled_text(
            self.label.to_owned(),
            metrics.titlebar_text,
            self.skin,
        ))
        .padding([0.0, metrics.titlebar_padding_x])
        .width(Length::Fill)
        .height(Length::Fill)
        .align_y(Vertical::Center);

        button(content)
            .padding(0.0)
            .width(Length::Fill)
            .height(Length::Fixed(metrics.titlebar_height))
            .style(move |_theme: &Theme, _status| ButtonStyle {
                background: Some(Background::Color(background)),
                ..ButtonStyle::default()
            })
            .on_press(UiEvent::Window(WindowCommand::Drag))
            .into()
    }
}
