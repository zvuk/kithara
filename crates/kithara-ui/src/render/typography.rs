use std::iter::once;

use iced::{
    Color, Element,
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
    text_in_role(content, role, None)
}

pub(crate) fn styled_text(
    content: String,
    role: TextRoleSkin,
    skin: &Skin,
) -> Element<'static, UiEvent> {
    text_in_role(content, role, Some(skin.color(role.color)))
}

fn text_in_role<'a, T: text::IntoFragment<'a> + AsRef<str>>(
    content: T,
    role: TextRoleSkin,
    color: Option<Color>,
) -> Element<'a, UiEvent> {
    if role.spacing <= 0.0 {
        return glyph(content, role, color).into();
    }
    let glyphs: Vec<_> = content
        .as_ref()
        .chars()
        .map(|char| glyph(char.to_string(), role, color).into())
        .collect();
    // The trailing zero-width child makes the Row spacing follow the last glyph,
    // so a tracked run keeps the same advance on both sides.
    Row::with_children(
        glyphs
            .into_iter()
            .chain(once(Space::new().width(0.0).into())),
    )
    .spacing(role.spacing * role.size)
    .into()
}

fn glyph<'a, T: text::IntoFragment<'a>>(
    fragment: T,
    role: TextRoleSkin,
    color: Option<Color>,
) -> Text<'a> {
    let text = shaped_text(fragment)
        .font(fonts::family(role.font, role.weight))
        .size(role.size);
    match color {
        Some(color) => text.color(color),
        None => text,
    }
}
