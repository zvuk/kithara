use iced::{
    Element, Length,
    alignment::Vertical,
    widget::{container, mouse_area},
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
        let content = container(styled_text(
            self.label.to_owned(),
            metrics.titlebar_text,
            self.skin,
        ))
        .padding([0.0, metrics.titlebar_padding_x])
        .width(Length::Fill)
        .height(Length::Fill)
        .align_y(Vertical::Center);

        mouse_area(content)
            .on_press(UiEvent::Window(WindowCommand::Drag))
            .into()
    }
}
