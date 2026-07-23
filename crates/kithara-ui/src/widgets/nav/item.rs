use iced::{
    Alignment, Background, Border, Element, Length, Theme,
    widget::{
        Space, button,
        button::{Status as ButtonStatus, Style as ButtonStyle},
        container,
        container::Style as ContainerStyle,
        row,
    },
};

use crate::{
    render::{ControlAction, Icon, ReadValue, Skin, UiEvent, fonts, shaped_text},
    widgets::Widget,
};

#[derive(bon::Builder)]
pub(crate) struct NavItem<'a, 'value, 'data, 'skin> {
    path: &'a str,
    label: &'a str,
    icon: Icon,
    value: Option<&'value ReadValue<'data>>,
    skin: &'skin Skin,
}

impl<'a> Widget<'a> for NavItem<'a, '_, '_, '_> {
    fn view(self) -> Element<'a, UiEvent> {
        let Some(ReadValue::Bool(active)) = self.value else {
            return Space::new().into();
        };
        let active = *active;
        let palette = self.skin.palette;
        let color = if active {
            palette.text
        } else {
            palette.text_dim
        };
        let marker = container(Space::new())
            .width(Length::Fixed(self.skin.nav.marker_width))
            .height(Length::Fill)
            .style(move |_| {
                ContainerStyle::default().background(Background::Color(if active {
                    palette.accent
                } else {
                    iced::Color::TRANSPARENT
                }))
            });
        let content = container(
            row![
                self.icon.view(self.skin.nav.icon_size, color),
                shaped_text(self.label)
                    .font(fonts::MONO)
                    .size(self.skin.nav.text_size)
                    .color(color),
            ]
            .spacing(self.skin.nav.icon_gap)
            .align_y(Alignment::Center),
        )
        .padding([self.skin.nav.pad_y, self.skin.nav.text_pad_x])
        .width(Length::Fill)
        .height(Length::Fill)
        .align_y(iced::alignment::Vertical::Center);

        button(row![marker, content].height(Length::Fill))
            .padding(self.skin.nav.pad_y)
            .width(Length::Fill)
            .height(Length::Fixed(self.skin.nav.item_height))
            .style(nav_item_style(self.skin, active))
            .on_press(UiEvent::Control {
                path: self.path.to_owned(),
                action: ControlAction::Activate,
            })
            .into()
    }
}

fn nav_item_style(
    skin: &Skin,
    active: bool,
) -> impl Fn(&Theme, ButtonStatus) -> ButtonStyle + 'static {
    let palette = skin.palette;
    move |_theme, _status| ButtonStyle {
        background: active.then_some(Background::Color(palette.bg_select)),
        text_color: if active {
            palette.text
        } else {
            palette.text_dim
        },
        border: Border::default(),
        ..ButtonStyle::default()
    }
}
