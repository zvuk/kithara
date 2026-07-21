use iced::{
    Background, Color, Element, Length,
    widget::{Column, Space, container, container::Style as ContainerStyle},
};

use crate::{
    render::{Skin, UiEvent, typography::styled_text},
    skin::ColorRole,
    widgets::Widget,
};

#[derive(bon::Builder)]
pub(crate) struct Swatch<'a, 'skin> {
    role: ColorRole,
    label: &'a str,
    skin: &'skin Skin,
}

impl<'a> Widget<'a> for Swatch<'a, '_> {
    fn view(self) -> Element<'a, UiEvent> {
        let metrics = self.skin.swatch;
        let color = self.skin.color(self.role);
        let border = self.skin.border(metrics.frame);
        let box_view = container(Space::new())
            .width(Length::Fill)
            .height(Length::Fixed(metrics.box_height))
            .style(move |_| {
                ContainerStyle::default()
                    .background(Background::Color(color))
                    .border(border)
            });
        let captions = Column::with_children([
            styled_text(self.label.to_uppercase(), metrics.label, self.skin),
            styled_text(format_hex(color), metrics.hex, self.skin),
        ])
        .spacing(metrics.label_hex_gap)
        .width(Length::Fill);
        Column::with_children([box_view.into(), captions.into()])
            .spacing(metrics.box_label_gap)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }
}

fn format_hex(color: Color) -> String {
    let [red, green, blue, _] = color.into_rgba8();
    format!("#{red:02X}{green:02X}{blue:02X}")
}

#[cfg(test)]
mod tests {
    use iced::Color;
    use kithara_test_utils::kithara;

    use super::format_hex;

    #[kithara::test]
    fn formats_color_as_uppercase_rgb_hex() {
        assert_eq!(format_hex(Color::from_rgb8(0x2e, 0xc7, 0xeb)), "#2EC7EB");
    }
}
