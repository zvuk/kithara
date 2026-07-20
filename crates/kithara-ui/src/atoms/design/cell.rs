use iced::{
    Background, Element, Length,
    widget::{Column, Space, container, container::Style as ContainerStyle},
};

use crate::{
    render::{Skin, UiEvent, typography::styled_text},
    widgets::Widget,
};

#[derive(bon::Builder)]
pub(crate) struct Cell<'a, 'skin> {
    label: Option<&'a str>,
    highlighted: bool,
    skin: &'skin Skin,
}

impl<'a> Widget<'a> for Cell<'a, '_> {
    fn view(self) -> Element<'a, UiEvent> {
        let metrics = self.skin.cell;
        let frame = if self.highlighted {
            metrics.highlighted_frame
        } else {
            metrics.frame
        };
        let border = self.skin.border(frame);
        let background = self.skin.color(metrics.background);
        let box_view = container(Space::new())
            .width(Length::Fill)
            .height(Length::Fill)
            .style(move |_| {
                ContainerStyle::default()
                    .background(Background::Color(background))
                    .border(border)
            });
        let Some(label) = self.label else {
            return box_view.into();
        };
        let mut role = self.skin.text.micro_label;
        if self.highlighted {
            role.color = metrics.highlighted_frame.border;
        }
        Column::with_children([
            box_view.into(),
            container(styled_text(label.to_uppercase(), role, self.skin))
                .width(Length::Fill)
                .height(Length::Fixed(metrics.label_height))
                .center_x(Length::Fill)
                .into(),
        ])
        .spacing(metrics.label_gap)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
    }
}
