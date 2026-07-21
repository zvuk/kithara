use iced::{
    Background, Border, Element, Length, Theme,
    widget::{
        Space, button,
        button::{Status as ButtonStatus, Style as ButtonStyle},
        column, container,
        container::Style as ContainerStyle,
    },
};

use crate::{
    render::{ControlAction, ReadValue, Skin, UiEvent, fonts, shaped_text},
    widgets::Widget,
};

#[derive(bon::Builder)]
pub(crate) struct TabLarge<'a, 'value, 'data, 'skin> {
    path: &'a str,
    label: &'a str,
    value: Option<&'value ReadValue<'data>>,
    skin: &'skin Skin,
}

impl<'a> Widget<'a> for TabLarge<'a, '_, '_, '_> {
    fn view(self) -> Element<'a, UiEvent> {
        let Some(ReadValue::Bool(active)) = self.value else {
            return Space::new().into();
        };
        let active = *active;
        let palette = self.skin.palette;
        let label = container(
            shaped_text(self.label)
                .font(fonts::MONO)
                .size(self.skin.tab_large.text_size)
                .color(if active {
                    palette.text
                } else {
                    palette.text_dim
                }),
        )
        .height(Length::Fill)
        .center_y(Length::Fill);
        let underline = container(Space::new())
            .width(Length::Fill)
            .height(Length::Fixed(self.skin.tab_large.underline_width))
            .style(move |_| {
                ContainerStyle::default().background(Background::Color(if active {
                    palette.accent
                } else {
                    iced::Color::TRANSPARENT
                }))
            });

        button(column![label, underline].height(Length::Fill))
            .padding([self.skin.tab_large.pad_y, self.skin.tab_large.pad_x])
            .width(Length::Shrink)
            .height(Length::Fixed(self.skin.tab_large.height))
            .style(tab_large_style(self.skin))
            .on_press(UiEvent::Control {
                path: self.path.to_owned(),
                action: ControlAction::Activate,
            })
            .into()
    }
}

fn tab_large_style(skin: &Skin) -> impl Fn(&Theme, ButtonStatus) -> ButtonStyle + 'static {
    let text_color = skin.palette.text;
    move |_theme, _status| ButtonStyle {
        background: None,
        text_color,
        border: Border::default(),
        ..ButtonStyle::default()
    }
}
