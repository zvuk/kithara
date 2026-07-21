use iced::{
    Element,
    widget::{Row, Text, text},
};

use crate::{
    render::{Skin, UiEvent, fonts},
    skin::TextRoleSkin,
};

/// Creates text with advanced shaping enabled.
pub fn shaped_text<'a, T: text::IntoFragment<'a>>(content: T) -> Text<'a> {
    Text::new(content).shaping(text::Shaping::Advanced)
}

pub(crate) fn styled_text(
    content: String,
    role: TextRoleSkin,
    skin: &Skin,
) -> Element<'static, UiEvent> {
    let font = fonts::family(role.font, role.weight);
    let color = skin.color(role.color);
    if role.spacing <= 0.0 {
        return shaped_text(content)
            .font(font)
            .size(role.size)
            .color(color)
            .into();
    }
    let glyphs = content.chars().map(|glyph| {
        shaped_text(glyph.to_string())
            .font(font)
            .size(role.size)
            .color(color)
            .into()
    });
    Row::with_children(glyphs)
        .spacing(role.spacing * role.size)
        .into()
}
