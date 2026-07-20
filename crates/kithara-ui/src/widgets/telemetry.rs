use iced::{
    Background, Element, Length,
    widget::{Space, container, container::Style as ContainerStyle},
};

use crate::{
    module::ScalarFormat,
    render::{ReadValue, Skin, UiEvent, fonts, shaped_text},
    widgets::Widget,
};

#[derive(bon::Builder)]
pub(crate) struct Telemetry<'value, 'data, 'skin> {
    format: ScalarFormat,
    value: Option<&'value ReadValue<'data>>,
    skin: &'skin Skin,
}

impl<'a> Widget<'a> for Telemetry<'_, '_, '_> {
    fn view(self) -> Element<'a, UiEvent> {
        let Some(ReadValue::Scalar(value)) = self.value else {
            return Space::new().into();
        };
        let formatted = if self.format == ScalarFormat::Percent {
            format!(
                "{value:>width$.precision$}%",
                value = *value * self.skin.telemetry.percent_scale,
                width = self.skin.telemetry.percent_width,
                precision = self.skin.telemetry.percent_precision,
            )
        } else {
            format!(
                "{value:.precision$}",
                precision = self.skin.telemetry.scalar_precision,
            )
        };
        let palette = self.skin.palette;
        let border = self.skin.border(self.skin.telemetry.frame);
        container(
            shaped_text(formatted)
                .font(fonts::mono(self.skin.telemetry.text.weight))
                .size(self.skin.telemetry.text.size)
                .color(palette.text),
        )
        .padding([self.skin.telemetry.padding_y, self.skin.telemetry.padding_x])
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
}
