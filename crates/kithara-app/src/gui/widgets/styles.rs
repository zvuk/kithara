use iced::{
    Background, Border, Color, Theme,
    widget::slider::{Handle, HandleShape, Rail, Status as SliderStatus, Style as SliderStyle},
};

use crate::theme::gui::GuiPalette;

pub(crate) const SLIDER_RAIL_WIDTH: f32 = 4.0;

pub(crate) fn slider_style(p: GuiPalette) -> impl Fn(&Theme, SliderStatus) -> SliderStyle {
    const ACTIVE_RAIL_ALPHA: f32 = 0.95;
    const DRAGGED_RAIL_ALPHA: f32 = 0.85;
    const HANDLE_BORDER_ALPHA: f32 = 0.65;
    const HANDLE_BORDER_WIDTH: f32 = 1.0;
    const HANDLE_RADIUS: f32 = 7.0;
    const INACTIVE_RAIL_ALPHA: f32 = 0.35;
    const RAIL_RADIUS: f32 = 4.0;

    move |_theme, status| {
        let active = match status {
            SliderStatus::Active => p.accent,
            SliderStatus::Hovered => with_alpha(p.accent, ACTIVE_RAIL_ALPHA),
            SliderStatus::Dragged => with_alpha(p.accent, DRAGGED_RAIL_ALPHA),
        };

        SliderStyle {
            rail: Rail {
                backgrounds: (
                    Background::Color(active),
                    Background::Color(with_alpha(p.muted, INACTIVE_RAIL_ALPHA)),
                ),
                width: SLIDER_RAIL_WIDTH,
                border: Border::default().rounded(RAIL_RADIUS),
            },
            handle: Handle {
                shape: HandleShape::Circle {
                    radius: HANDLE_RADIUS,
                },
                background: Background::Color(p.text),
                border_width: HANDLE_BORDER_WIDTH,
                border_color: with_alpha(p.bg, HANDLE_BORDER_ALPHA),
            },
        }
    }
}

fn with_alpha(color: Color, alpha: f32) -> Color {
    Color { a: alpha, ..color }
}
