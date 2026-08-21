use iced::{
    Background, Element, Length,
    alignment::{Horizontal, Vertical},
    widget::{Space, container, container::Style as ContainerStyle},
};

use crate::{
    module::ScalarFormat,
    render::{ReadValue, Skin, UiEvent, typography::styled_text},
    widgets::Widget,
};

#[derive(bon::Builder)]
pub(crate) struct Telemetry<'value, 'data, 'skin> {
    skin: &'skin Skin,
    value: Option<&'value ReadValue<'data>>,
    format: ScalarFormat,
    framed: bool,
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
        let readout = container(styled_text(formatted, self.skin.telemetry.text, self.skin))
            .height(Length::Fill)
            .align_x(Horizontal::Center)
            .align_y(Vertical::Center);
        if !self.framed {
            return readout.width(Length::Shrink).into();
        }
        let border = self.skin.border(self.skin.telemetry.frame);
        readout
            .padding([self.skin.telemetry.padding_y, self.skin.telemetry.padding_x])
            .width(Length::Fill)
            .style(move |_| {
                ContainerStyle::default()
                    .background(Background::Color(palette.bg_inset))
                    .border(border)
            })
            .into()
    }
}
