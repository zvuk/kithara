use iced::{
    Background, Border, Theme,
    widget::{
        slider::{Handle, HandleShape, Rail, Status as SliderStatus, Style as SliderStyle},
        text_input::{Status as TextInputStatus, Style as TextInputStyle},
    },
};
use kithara_ui::render::RenderPalette;

use crate::gui::tokens::{chrome, volume};

pub(crate) fn slider_style(p: RenderPalette) -> impl Fn(&Theme, SliderStatus) -> SliderStyle {
    move |_theme, _status| SliderStyle {
        rail: Rail {
            backgrounds: (Background::Color(p.accent), Background::Color(p.bg_deep)),
            width: volume::RAIL_WIDTH,
            border: Border::default().width(1).color(p.line_soft),
        },
        handle: Handle {
            shape: HandleShape::Rectangle {
                width: volume::HANDLE_WIDTH,
                border_radius: 0.0.into(),
            },
            background: Background::Color(p.bg_panel_2),
            border_width: volume::HANDLE_BORDER_WIDTH,
            border_color: p.line,
        },
    }
}

pub(crate) fn text_input_style(
    p: RenderPalette,
) -> impl Fn(&Theme, TextInputStatus) -> TextInputStyle {
    move |_theme, status| TextInputStyle {
        background: Background::Color(p.bg_inset),
        border: Border::default()
            .width(if matches!(status, TextInputStatus::Focused { .. }) {
                chrome::BORDER_WIDTH
            } else {
                0.0
            })
            .color(p.accent),
        icon: p.muted,
        placeholder: p.muted,
        value: p.text,
        selection: p.accent_soft,
    }
}
