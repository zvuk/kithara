use iced::{
    Border, Element, Length,
    widget::{Column, Space, container, container::Style as ContainerStyle},
};

use crate::{
    registry::{ControlKindDesc, PropKind, ValueKind},
    render::{ReadValue, RenderPalette, UiEvent, fonts, shaped_text},
    size::{Dim, SizeSpec},
};

struct Consts;

impl Consts {
    const BORDER_WIDTH: f32 = 1.0;
    const HEIGHT: f32 = 26.0;
    const LABEL_SIZE: f32 = 8.0;
    const MIN_WIDTH: f32 = 40.0;
    const PADDING_X: f32 = 6.0;
    const SPACING: f32 = 2.0;
    const VALUE_SIZE: f32 = 11.0;
}

pub(crate) fn desc() -> ControlKindDesc {
    ControlKindDesc::new(Some(ValueKind::Text), None)
        .with_prop("label", PropKind::Text)
        .with_prop("tone", PropKind::Text)
        .with_prop("framed", PropKind::Bool)
        .with_size(SizeSpec::new(
            Dim::Range {
                min: Consts::MIN_WIDTH,
                max: None,
            },
            Dim::Fixed(Consts::HEIGHT),
        ))
}

pub(crate) fn view<'a>(
    label: Option<&'a str>,
    tone: Option<&str>,
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
    let value_color = if tone == Some("accent") {
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
