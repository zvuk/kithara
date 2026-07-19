use iced::{
    Background, Border, Element, Length, Theme,
    widget::{
        Space, button,
        button::{Status as ButtonStatus, Style as ButtonStyle},
    },
};

use crate::render::{ControlAction, ReadValue, RenderPalette, UiEvent, fonts, shaped_text};

struct Consts;

impl Consts {
    const BORDER_WIDTH: f32 = 1.0;
    const PADDING_X: f32 = 8.0;
    const PADDING_Y: f32 = 3.0;
    const TEXT_SIZE: f32 = 9.0;
}

pub(crate) fn view<'a>(
    path: &str,
    label: &'a str,
    value: Option<&ReadValue<'_>>,
    palette: RenderPalette,
) -> Element<'a, UiEvent> {
    let Some(ReadValue::Bool(active)) = value else {
        return Space::new().into();
    };

    button(shaped_text(label).font(fonts::MONO).size(Consts::TEXT_SIZE))
        .padding([Consts::PADDING_Y, Consts::PADDING_X])
        .width(Length::Fill)
        .height(Length::Fill)
        .style(chip_style(palette, *active))
        .on_press(UiEvent::Control {
            path: path.to_owned(),
            action: ControlAction::Activate,
        })
        .into()
}

fn chip_style(
    palette: RenderPalette,
    active: bool,
) -> impl Fn(&Theme, ButtonStatus) -> ButtonStyle {
    move |_theme, _status| ButtonStyle {
        background: active.then_some(Background::Color(palette.accent)),
        text_color: if active {
            palette.bg_deep
        } else {
            palette.text_dim
        },
        border: if active {
            Border::default()
        } else {
            Border::default()
                .width(Consts::BORDER_WIDTH)
                .color(palette.line)
        },
        ..ButtonStyle::default()
    }
}
