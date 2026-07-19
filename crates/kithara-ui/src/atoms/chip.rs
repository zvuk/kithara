use iced::{
    Background, Element, Length, Theme,
    widget::{
        Space, button,
        button::{Status as ButtonStatus, Style as ButtonStyle},
    },
};

use crate::render::{ControlAction, ReadValue, Skin, UiEvent, fonts, shaped_text};

pub(crate) fn view<'a>(
    path: &str,
    label: &'a str,
    value: Option<&ReadValue<'_>>,
    skin: &Skin,
) -> Element<'a, UiEvent> {
    let Some(ReadValue::Bool(active)) = value else {
        return Space::new().into();
    };

    button(
        shaped_text(label)
            .font(fonts::mono(skin.chip.text.weight))
            .size(skin.chip.text.size),
    )
    .padding([skin.chip.padding_y, skin.chip.padding_x])
    .width(Length::Fill)
    .height(Length::Fill)
    .style(chip_style(skin, *active))
    .on_press(UiEvent::Control {
        path: path.to_owned(),
        action: ControlAction::Activate,
    })
    .into()
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
