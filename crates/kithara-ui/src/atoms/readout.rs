use iced::{
    Element, Length,
    widget::{Column, Space, container, container::Style as ContainerStyle},
};

use crate::{
    module::Tone,
    render::{ReadValue, Skin, UiEvent, fonts, shaped_text},
};

pub(crate) fn view<'a>(
    label: Option<&'a str>,
    tone: Tone,
    framed: bool,
    value: Option<&ReadValue<'_>>,
    skin: &Skin,
) -> Element<'a, UiEvent> {
    let Some(label) = label else {
        return Space::new().into();
    };
    let value = match value {
        Some(ReadValue::Text(value)) => (*value).to_owned(),
        Some(ReadValue::Scalar(value)) => format!("{value:.2}"),
        _ => return Space::new().into(),
    };
    let palette = skin.palette;
    let value_color = if tone == Tone::Accent {
        palette.accent
    } else {
        palette.text
    };
    let content = Column::with_children([
        shaped_text(label)
            .font(fonts::mono(skin.readout.label.weight))
            .size(skin.readout.label.size)
            .color(palette.muted)
            .into(),
        shaped_text(value)
            .font(fonts::mono(skin.readout.value.weight))
            .size(skin.readout.value.size)
            .color(value_color)
            .into(),
    ])
    .spacing(skin.readout.spacing);

    let border = skin.border(skin.readout.frame);

    container(content)
        .padding([
            skin.readout.padding_y,
            if framed { skin.readout.padding_x } else { 0.0 },
        ])
        .width(Length::Fill)
        .height(Length::Fill)
        .style(move |_| {
            let style = ContainerStyle::default();
            if framed { style.border(border) } else { style }
        })
        .into()
}
