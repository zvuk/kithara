use iced::{
    Background, Element, Length,
    widget::{Space, container, container::Style as ContainerStyle},
};

use crate::{
    module::ScalarFormat,
    render::{ReadValue, Skin, UiEvent, fonts, shaped_text},
};

pub(crate) fn view<'a>(
    format: ScalarFormat,
    value: Option<&ReadValue<'_>>,
    skin: &Skin,
) -> Element<'a, UiEvent> {
    let Some(ReadValue::Scalar(value)) = value else {
        return Space::new().into();
    };
    let formatted = if format == ScalarFormat::Percent {
        format!(
            "{value:>width$.precision$}%",
            value = *value * skin.telemetry.percent_scale,
            width = skin.telemetry.percent_width,
            precision = skin.telemetry.percent_precision,
        )
    } else {
        format!(
            "{value:.precision$}",
            precision = skin.telemetry.scalar_precision,
        )
    };
    let palette = skin.palette;
    let border = skin.border(skin.telemetry.frame);
    container(
        shaped_text(formatted)
            .font(fonts::mono(skin.telemetry.text.weight))
            .size(skin.telemetry.text.size)
            .color(palette.text),
    )
    .padding([skin.telemetry.padding_y, skin.telemetry.padding_x])
    .width(Length::Fill)
    .height(Length::Fill)
    .center_x(Length::Fill)
    .center_y(Length::Fill)
    .style(move |_| {
        ContainerStyle::default()
            .background(Background::Color(palette.bg_inset))
            .border(border)
    })
    .into()
}
