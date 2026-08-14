use iced::{
    Element,
    widget::{
        Row, Space, Text,
        text::{self, Shaping},
    },
};

use crate::{
    render::{Skin, UiEvent, fonts},
    skin::TextRoleSkin,
};

/// Creates text with advanced shaping enabled.
pub fn shaped_text<'a, T: text::IntoFragment<'a>>(content: T) -> Text<'a> {
    Text::new(content).shaping(Shaping::Advanced)
}

pub(crate) fn tracked_text<'a>(content: &'a str, role: TextRoleSkin) -> Element<'a, UiEvent> {
    let font = fonts::family(role.font, role.weight);
    if role.spacing <= 0.0 {
        return shaped_text(content).font(font).size(role.size).into();
    }
    let glyphs = content
        .chars()
        .map(|glyph| {
            shaped_text(glyph.to_string())
                .font(font)
                .size(role.size)
                .into()
        })
        .chain(std::iter::once(Space::new().width(0.0).into()));
    Row::with_children(glyphs)
        .spacing(role.spacing * role.size)
        .into()
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
    let glyphs = content
        .chars()
        .map(|glyph| {
            shaped_text(glyph.to_string())
                .font(font)
                .size(role.size)
                .color(color)
                .into()
        })
        .chain(std::iter::once(Space::new().width(0.0).into()));
    Row::with_children(glyphs)
        .spacing(role.spacing * role.size)
        .into()
}
