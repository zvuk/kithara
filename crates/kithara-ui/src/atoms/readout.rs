use iced::{
    Border, Element, Length,
    widget::{Column, Space, container, container::Style as ContainerStyle},
};

use crate::{
    module::Tone,
    render::{ReadValue, RenderPalette, UiEvent, fonts, shaped_text},
};

struct Consts;

impl Consts {
    const BORDER_WIDTH: f32 = 1.0;
    const LABEL_SIZE: f32 = 8.0;
    const PADDING_X: f32 = 6.0;
    const SPACING: f32 = 2.0;
    const VALUE_SIZE: f32 = 11.0;
}

pub(crate) fn view<'a>(
    label: Option<&'a str>,
    tone: Tone,
    framed: bool,
    value: Option<&ReadValue<'_>>,
    palette: RenderPalette,
) -> Element<'a, UiEvent> {
    let Some(label) = label else {
        return Space::new().into();
    };
    let value = match value {
        Some(ReadValue::Text(value)) => (*value).to_owned(),
        Some(ReadValue::Scalar(value)) => format!("{value:.2}"),
        _ => return Space::new().into(),
    };
    let value_color = if tone == Tone::Accent {
        palette.accent
    } else {
        palette.text
    };
    let content = Column::with_children([
        shaped_text(label)
            .font(fonts::MONO)
            .size(Consts::LABEL_SIZE)
            .color(palette.muted)
            .into(),
        shaped_text(value)
            .font(fonts::MONO)
            .size(Consts::VALUE_SIZE)
            .color(value_color)
            .into(),
    ])
    .spacing(Consts::SPACING);

    container(content)
        .padding([0.0, if framed { Consts::PADDING_X } else { 0.0 }])
        .width(Length::Fill)
        .height(Length::Fill)
        .style(move |_| {
            let style = ContainerStyle::default();
            if framed {
                style.border(
                    Border::default()
                        .width(Consts::BORDER_WIDTH)
                        .color(palette.line),
                )
            } else {
                style
            }
        })
        .into()
}
