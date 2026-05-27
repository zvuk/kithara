use iced::{
    Background, Border, Color, Element, Length, Shadow, Theme, Vector,
    widget::{
        button,
        button::{Status as ButtonStatus, Style as ButtonStyle},
        container,
    },
};

use crate::{
    gui::{icons::Icon, message::Message, view::mix_colors, view::with_alpha},
    theme::gui::GuiPalette,
};

const SIZE: f32 = 56.0;
const ICON_SIZE: f32 = 22.0;
const BORDER_RADIUS: f32 = 8.0;
const BORDER_WIDTH: f32 = 1.0;
const ALPHA_DISABLED: f32 = 0.4;
const ALPHA_GLOW_HOVER: f32 = 0.30;
const ALPHA_GLOW_REST: f32 = 0.22;
const GLOW_HOVER_BLUR: f32 = 11.0;
const GLOW_HOVER_OFFSET_Y: f32 = 4.0;
const GLOW_REST_BLUR: f32 = 4.0;
const GLOW_REST_OFFSET_Y: f32 = 1.0;
const BORDER_DARKEN_MIX: f32 = 0.30;

/// Primary play/pause control shared by compact view and DJ Studio:
/// a solid-gold rounded square with a hover glow. The icon swaps
/// between play and pause based on `playing`.
pub(crate) fn play_button(
    playing: bool,
    p: GuiPalette,
    message: Message,
) -> Element<'static, Message> {
    let icon = if playing { Icon::Pause } else { Icon::Play };
    button(
        container(icon.view(ICON_SIZE, p.bg))
            .center_x(Length::Fill)
            .center_y(Length::Fill),
    )
    .width(Length::Fixed(SIZE))
    .height(Length::Fixed(SIZE))
    .padding(0)
    .style(move |_theme: &Theme, status| play_style(p, status))
    .on_press(message)
    .into()
}

fn play_style(p: GuiPalette, status: ButtonStatus) -> ButtonStyle {
    // Solid gold, not a gradient: iced_wgpu 0.14 renders shadows only on the
    // solid quad pipeline; the gradient shader drops shadow entirely.
    let background = match status {
        ButtonStatus::Hovered | ButtonStatus::Pressed => p.accent_strong,
        ButtonStatus::Disabled => with_alpha(p.accent, ALPHA_DISABLED),
        ButtonStatus::Active => p.accent,
    };

    let shadow = match status {
        ButtonStatus::Hovered | ButtonStatus::Pressed => Shadow {
            color: with_alpha(p.accent, ALPHA_GLOW_HOVER),
            offset: Vector::new(0.0, GLOW_HOVER_OFFSET_Y),
            blur_radius: GLOW_HOVER_BLUR,
        },
        ButtonStatus::Disabled => Shadow::default(),
        ButtonStatus::Active => Shadow {
            color: with_alpha(p.accent, ALPHA_GLOW_REST),
            offset: Vector::new(0.0, GLOW_REST_OFFSET_Y),
            blur_radius: GLOW_REST_BLUR,
        },
    };

    ButtonStyle {
        background: Some(Background::Color(background)),
        text_color: p.bg,
        border: Border::default()
            .rounded(BORDER_RADIUS)
            .width(BORDER_WIDTH)
            .color(mix_colors(p.accent, Color::BLACK, BORDER_DARKEN_MIX)),
        shadow,
        ..ButtonStyle::default()
    }
}
