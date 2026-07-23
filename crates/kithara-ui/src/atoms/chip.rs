use iced::{
    Background, Element, Length, Theme,
    widget::{
        Space, button,
        button::{Status as ButtonStatus, Style as ButtonStyle},
    },
};

use crate::{
    module::ChipStyle,
    render::{ControlAction, ReadValue, Skin, UiEvent, fonts, shaped_text},
    widgets::Widget,
};

#[derive(bon::Builder)]
pub(crate) struct Chip<'a, 'value, 'data, 'skin> {
    path: &'a str,
    label: &'a str,
    style: ChipStyle,
    value: Option<&'value ReadValue<'data>>,
    skin: &'skin Skin,
}

impl<'a> Widget<'a> for Chip<'a, '_, '_, '_> {
    fn view(self) -> Element<'a, UiEvent> {
        let Some(ReadValue::Bool(active)) = self.value else {
            return Space::new().into();
        };
        let text = match self.style {
            ChipStyle::Deck => self.skin.chip.deck_text,
            ChipStyle::Routing => self.skin.chip.routing_text,
        };
        button(
            shaped_text(self.label)
                .font(fonts::mono(text.weight))
                .size(text.size),
        )
        .padding([self.skin.chip.padding_y, self.skin.chip.padding_x])
        .width(Length::Fill)
        .height(Length::Fill)
        .style(chip_style(self.skin, *active))
        .on_press(UiEvent::Control {
            path: self.path.to_owned(),
            action: ControlAction::Activate,
        })
        .into()
    }
}

fn chip_style(skin: &Skin, active: bool) -> impl Fn(&Theme, ButtonStatus) -> ButtonStyle + 'static {
    let palette = skin.palette;
    let border = skin.border(if active {
        skin.chip.active_frame
    } else {
        skin.chip.inactive_frame
    });
    move |_theme, _status| ButtonStyle {
        background: active.then_some(Background::Color(palette.accent)),
        text_color: if active {
            palette.bg_deep
        } else {
            palette.text_dim
        },
        border,
        ..ButtonStyle::default()
    }
}
