use iced::{
    Element, Length,
    widget::{Column, Space, container, container::Style as ContainerStyle},
};

use crate::{
    module::Tone,
    render::{ReadValue, Skin, UiEvent, fonts, shaped_text},
    widgets::Widget,
};

#[derive(bon::Builder)]
pub(crate) struct Readout<'a, 'value, 'data, 'skin> {
    label: Option<&'a str>,
    tone: Tone,
    framed: bool,
    value: Option<&'value ReadValue<'data>>,
    skin: &'skin Skin,
}

impl<'a> Widget<'a> for Readout<'a, '_, '_, '_> {
    fn view(self) -> Element<'a, UiEvent> {
        let Some(label) = self.label else {
            return Space::new().into();
        };
        let value = match self.value {
            Some(ReadValue::Text(value)) => (*value).to_owned(),
            Some(ReadValue::Scalar(value)) => format!("{value:.2}"),
            _ => return Space::new().into(),
        };
        let palette = self.skin.palette;
        let value_color = match self.tone {
            Tone::Neutral => palette.text,
            Tone::Accent => palette.accent,
            Tone::Success => palette.success,
            Tone::Danger => palette.danger,
        };
        let content = Column::with_children([
            shaped_text(label)
                .font(fonts::mono(self.skin.readout.label.weight))
                .size(self.skin.readout.label.size)
                .color(palette.muted)
                .into(),
            shaped_text(value)
                .font(fonts::mono(self.skin.readout.value.weight))
                .size(self.skin.readout.value.size)
                .color(value_color)
                .into(),
        ])
        .spacing(self.skin.readout.spacing);
        let border = self.skin.border(self.skin.readout.frame);

        container(content)
            .padding([
                self.skin.readout.padding_y,
                if self.framed {
                    self.skin.readout.padding_x
                } else {
                    0.0
                },
            ])
            .width(Length::Fill)
            .height(Length::Fill)
            .style(move |_| {
                let style = ContainerStyle::default();
                if self.framed {
                    style.border(border)
                } else {
                    style
                }
            })
            .into()
    }
}
