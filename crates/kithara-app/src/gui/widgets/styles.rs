use iced::{
    Background, Border, Theme,
    widget::{
        slider::{Handle, HandleShape, Rail, Status as SliderStatus, Style as SliderStyle},
        text_input::{Status as TextInputStatus, Style as TextInputStyle},
    },
};

use crate::{
    gui::tokens::{chrome, volume},
    theme::gui::GuiPalette,
};

pub(crate) fn slider_style(p: GuiPalette) -> impl Fn(&Theme, SliderStatus) -> SliderStyle {
    move |_theme, _status| SliderStyle {
        rail: Rail {
            backgrounds: (
                Background::Color(p.accent),
                Background::Color(p.canvas.bg_deep),
            ),
            width: volume::RAIL_WIDTH,
            border: Border::default().width(1).color(p.canvas.line_soft),
        },
        handle: Handle {
            shape: HandleShape::Rectangle {
                width: volume::HANDLE_WIDTH,
                border_radius: 0.0.into(),
            },
            background: Background::Color(p.canvas.bg_panel_2),
            border_width: volume::HANDLE_BORDER_WIDTH,
            border_color: p.canvas.line,
        },
    }
}

pub(crate) fn text_input_style(
    p: GuiPalette,
) -> impl Fn(&Theme, TextInputStatus) -> TextInputStyle {
    move |_theme, status| TextInputStyle {
        background: Background::Color(p.canvas.bg_inset),
        border: Border::default()
            .width(if matches!(status, TextInputStatus::Focused { .. }) {
                chrome::BORDER_WIDTH
            } else {
                0.0
            })
            .color(p.accent),
        icon: p.canvas.muted,
        placeholder: p.canvas.muted,
        value: p.canvas.text,
        selection: p.accent_soft,
    }
}
