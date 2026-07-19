use iced::{
    Background, Border, Element, Length,
    widget::{Space, container, container::Style as ContainerStyle},
};

use crate::{
    registry::{ControlKindDesc, PropKind, ValueKind},
    render::{ReadValue, RenderPalette, UiEvent, fonts, shaped_text},
    size::{Dim, SizeSpec},
};

struct Consts;

impl Consts {
    const HEIGHT: f32 = 18.0;
    const PADDING_X: f32 = 7.0;
    const PADDING_Y: f32 = 4.0;
    const PERCENT_SCALE: f64 = 100.0;
    const TEXT_SIZE: f32 = 12.0;
    const WIDTH: f32 = 64.0;
}

pub(crate) fn desc() -> ControlKindDesc {
    ControlKindDesc::new(Some(ValueKind::Scalar), None)
        .with_prop("format", PropKind::Text)
        .with_size(SizeSpec::new(
            Dim::Fixed(Consts::WIDTH),
            Dim::Fixed(Consts::HEIGHT),
        ))
}

pub(crate) fn view<'a>(
    format: Option<&str>,
    value: Option<&ReadValue<'_>>,
    palette: RenderPalette,
) -> Element<'a, UiEvent> {
    let Some(ReadValue::Scalar(value)) = value else {
        return Space::new().into();
    };
    let formatted = if format == Some("percent") {
        format!("{:>3.0}%", *value * Consts::PERCENT_SCALE)
    } else {
        format!("{value:.2}")
    };
    container(
        shaped_text(formatted)
            .font(fonts::MONO)
            .size(Consts::TEXT_SIZE)
            .color(palette.text),
    )
    .padding([Consts::PADDING_Y, Consts::PADDING_X])
    .width(Length::Fill)
    .height(Length::Fill)
    .center_x(Length::Fill)
    .center_y(Length::Fill)
    .style(move |_| {
        ContainerStyle::default()
            .background(Background::Color(palette.bg_inset))
            .border(Border::default().width(1).color(palette.line))
    })
    .into()
}
