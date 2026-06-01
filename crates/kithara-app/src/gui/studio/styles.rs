use iced::{
    Background, Border, Color, Degrees, Element, Length, Theme, gradient,
    widget::{
        Space,
        button::{Status as ButtonStatus, Style as ButtonStyle},
        container,
        container::Style as ContainerStyle,
    },
};

use super::tokens::studio_radius;
pub(super) use crate::gui::view::mix_colors;
use crate::{
    gui::{message::Message, view::with_alpha},
    theme::gui::GuiPalette,
};

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

pub(super) fn shell_style(p: GuiPalette) -> impl Fn(&Theme) -> ContainerStyle {
    move |_theme| {
        ContainerStyle::default()
            .background(window_background(p))
            .color(p.text)
    }
}

pub(crate) fn ghost_button_style(p: GuiPalette) -> impl Fn(&Theme, ButtonStatus) -> ButtonStyle {
    move |_theme, status| {
        let background = match status {
            ButtonStatus::Active => Some(Background::Color(Color::TRANSPARENT)),
            ButtonStatus::Hovered => Some(Background::Color(with_alpha(p.bg_panel_2, 0.8))),
            ButtonStatus::Pressed => Some(Background::Color(p.accent_soft)),
            ButtonStatus::Disabled => Some(Background::Color(with_alpha(p.bg_panel, 0.35))),
        };

        ButtonStyle {
            background,
            text_color: p.text,
            border: Border::default()
                .rounded(studio_radius::BUTTON)
                .width(1.0)
                .color(p.line_soft),
            ..ButtonStyle::default()
        }
    }
}

pub(super) fn linear_background(angle: f32, start: Color, end: Color) -> Background {
    gradient::Linear::new(Degrees(angle))
        .add_stop(0.0, start)
        .add_stop(1.0, end)
        .into()
}

fn window_background(p: GuiPalette) -> Background {
    linear_background(
        180.0,
        with_alpha(p.bg, 0.96),
        with_alpha(mix_colors(p.bg, p.bg_deep, 0.42), 0.99),
    )
}
