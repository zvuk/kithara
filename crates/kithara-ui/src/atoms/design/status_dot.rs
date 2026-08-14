use iced::{
    Alignment, Background, Border, Element, Length,
    widget::{Row, Space, container, container::Style as ContainerStyle},
};

use crate::{
    module::Tone,
    render::{Skin, UiEvent, fonts, shaped_text},
    widgets::Widget,
};

#[derive(bon::Builder)]
pub(crate) struct StatusDot<'a, 'skin> {
    skin: &'skin Skin,
    label: &'a str,
    dot_size: Option<f32>,
    tone: Tone,
}

impl<'a> Widget<'a> for StatusDot<'a, '_> {
    fn view(self) -> Element<'a, UiEvent> {
        let metrics = self.skin.status_dot;
        let dot_color = match self.tone {
            Tone::Neutral => self.skin.palette.muted,
            Tone::Accent => self.skin.palette.accent,
            Tone::Success => self.skin.palette.success,
            Tone::Danger => self.skin.palette.danger,
        };
        let dot = container(Space::new())
            .width(Length::Fixed(self.dot_size.unwrap_or(metrics.dot_size)))
            .height(Length::Fixed(self.dot_size.unwrap_or(metrics.dot_size)))
            .style(move |_| {
                ContainerStyle::default()
                    .background(Background::Color(dot_color))
                    .border(Border::default())
            });
        if self.label.is_empty() {
            return dot.into();
        }
        Row::with_children([
            dot.into(),
            shaped_text(self.label)
                .font(fonts::mono(metrics.text.weight))
                .size(metrics.text.size)
                .color(self.skin.color(metrics.text_color))
                .into(),
        ])
        .spacing(metrics.gap)
        .align_y(Alignment::Center)
        .into()
    }
}
