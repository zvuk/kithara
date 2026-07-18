use iced::{
    Background, Border, Color, Element, Length, Theme,
    widget::{
        Space,
        button::{Status as ButtonStatus, Style as ButtonStyle},
        container,
        container::Style as ContainerStyle,
    },
};

use crate::{gui::message::Message, theme::gui::GuiPalette};

pub(super) fn vertical_divider(width: f32, height: f32, color: Color) -> Element<'static, Message> {
    container(Space::new())
        .width(Length::Fixed(width))
        .height(if height.is_finite() {
            Length::Fixed(height)
        } else {
            Length::Fill
        })
        .style(move |_theme: &Theme| ContainerStyle::default().background(color))
        .into()
}

pub(super) fn horizontal_divider(height: f32, color: Color) -> Element<'static, Message> {
    container(Space::new())
        .width(Length::Fill)
        .height(Length::Fixed(height))
        .style(move |_theme: &Theme| ContainerStyle::default().background(color))
        .into()
}

pub(super) fn shell_style(p: GuiPalette) -> impl Fn(&Theme) -> ContainerStyle {
    move |_theme| ContainerStyle::default().background(p.bg).color(p.text)
}

pub(crate) fn ghost_button_style(p: GuiPalette) -> impl Fn(&Theme, ButtonStatus) -> ButtonStyle {
    move |_theme, status| {
        let (background, text_color, border_color) = match status {
            ButtonStatus::Active => (Color::TRANSPARENT, p.text, p.line),
            ButtonStatus::Hovered => (Color::TRANSPARENT, p.text, p.text_dim),
            ButtonStatus::Pressed => (p.accent, p.bg, p.accent),
            ButtonStatus::Disabled => (Color::TRANSPARENT, p.muted, p.line_soft),
        };

        ButtonStyle {
            background: Some(Background::Color(background)),
            text_color,
            border: Border::default().width(1.0).color(border_color),
            ..ButtonStyle::default()
        }
    }
}
