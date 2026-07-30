use iced::{Element, Length, alignment::Vertical, widget::container};

use crate::{
    module::TextStyle,
    render::{ReadValue, Skin, UiEvent, typography::styled_text},
    skin::TextRoleSkin,
    widgets::Widget,
};

#[derive(bon::Builder)]
pub(crate) struct Text<'value, 'data, 'skin> {
    style: TextStyle,
    value: Option<&'value ReadValue<'data>>,
    label: Option<&'data str>,
    active: bool,
    skin: &'skin Skin,
}

impl<'a> Widget<'a> for Text<'_, '_, '_> {
    fn view(self) -> Element<'a, UiEvent> {
        let value = match self.value {
            Some(ReadValue::Text(value)) => Some(*value),
            _ => self.label,
        };
        let Some(value) = value else {
            return iced::widget::Space::new().into();
        };
        let role = text_role(self.style, self.active, self.skin);
        let content = if self.style == TextStyle::MicroLabel {
            value.to_uppercase()
        } else {
            value.to_owned()
        };
        let padding_x = match self.style {
            TextStyle::VisFooter => self.skin.vis.footer_padding_x,
            TextStyle::VisMeta => self.skin.vis.index_padding_x,
            TextStyle::VisTitle => self.skin.vis.name_padding_x,
            _ => 0.0,
        };
        container(styled_text(content, role, self.skin))
            .padding([0.0, padding_x])
            .height(Length::Fill)
            .align_y(Vertical::Center)
            .into()
    }
}

/// Joins a text style to its skin entry. A role that declares an active colour
/// takes it while the node's `active` binding reads true.
fn text_role(style: TextStyle, active: bool, skin: &Skin) -> TextRoleSkin {
    let (role, active_color) = match style {
        TextStyle::Body => (skin.text.body, None),
        TextStyle::Brand => (skin.text.brand, None),
        TextStyle::BrandSmall => (skin.text.brand_small, None),
        TextStyle::DeckLetter => (skin.text.deck_letter, Some(skin.text.deck_letter_active)),
        TextStyle::TrackTitle => (skin.text.track_title, None),
        TextStyle::Telemetry => (skin.text.telemetry, None),
        TextStyle::MicroLabel => (skin.text.micro_label, None),
        TextStyle::Section => (skin.text.section, None),
        TextStyle::MenuRow => (skin.menu.row, Some(skin.menu.row_active)),
        TextStyle::MenuRowStrong => (skin.menu.row_strong, None),
        TextStyle::MenuRowAccent => (skin.menu.row_accent, Some(skin.menu.row_accent_active)),
        TextStyle::MenuHint => (skin.menu.hint, None),
        TextStyle::MenuHintAccent => (skin.menu.hint_accent, None),
        TextStyle::MenuSection => (skin.menu.section, None),
        TextStyle::MenuCount => (skin.menu.count, None),
        TextStyle::MenuCaption => (skin.menu.caption, None),
        TextStyle::MenuList => (skin.menu.list, Some(skin.menu.list_active)),
        TextStyle::MenuCell => (skin.menu.cell, Some(skin.menu.cell_active)),
        TextStyle::VisFooter | TextStyle::VisMeta => (skin.vis.meta, None),
        TextStyle::VisTitle => (skin.vis.title, None),
    };
    active
        .then_some(active_color)
        .flatten()
        .map_or(role, |color| TextRoleSkin { color, ..role })
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;
    use crate::builtin;

    #[kithara::test]
    fn every_text_style_resolves_to_its_own_skin_role() {
        let skin = builtin::skin();

        for (style, role) in [
            (TextStyle::Body, skin.text.body),
            (TextStyle::Brand, skin.text.brand),
            (TextStyle::BrandSmall, skin.text.brand_small),
            (TextStyle::DeckLetter, skin.text.deck_letter),
            (TextStyle::TrackTitle, skin.text.track_title),
            (TextStyle::Telemetry, skin.text.telemetry),
            (TextStyle::MicroLabel, skin.text.micro_label),
            (TextStyle::Section, skin.text.section),
            (TextStyle::MenuRow, skin.menu.row),
            (TextStyle::MenuRowStrong, skin.menu.row_strong),
            (TextStyle::MenuRowAccent, skin.menu.row_accent),
            (TextStyle::MenuHint, skin.menu.hint),
            (TextStyle::MenuHintAccent, skin.menu.hint_accent),
            (TextStyle::MenuSection, skin.menu.section),
            (TextStyle::MenuCount, skin.menu.count),
            (TextStyle::MenuCaption, skin.menu.caption),
            (TextStyle::MenuList, skin.menu.list),
            (TextStyle::MenuCell, skin.menu.cell),
            (TextStyle::VisFooter, skin.vis.meta),
            (TextStyle::VisMeta, skin.vis.meta),
            (TextStyle::VisTitle, skin.vis.title),
        ] {
            assert_eq!(text_role(style, false, skin), role, "{style:?}");
        }
    }

    #[kithara::test]
    fn brand_small_resolves_under_text_and_never_under_menu() {
        let skin = builtin::skin();
        let role = text_role(TextStyle::BrandSmall, false, skin);

        assert_eq!(role, skin.text.brand_small);
        assert_eq!(text_role(TextStyle::BrandSmall, true, skin), role);
        assert_ne!(
            role.font, skin.menu.row.font,
            "every menu role shares one family, and the brand pair is not in it"
        );
    }

    #[kithara::test]
    fn a_role_takes_the_active_colour_its_skin_entry_declares() {
        let skin = builtin::skin();

        for (style, color) in [
            (TextStyle::DeckLetter, skin.text.deck_letter_active),
            (TextStyle::MenuRow, skin.menu.row_active),
            (TextStyle::MenuRowAccent, skin.menu.row_accent_active),
            (TextStyle::MenuList, skin.menu.list_active),
            (TextStyle::MenuCell, skin.menu.cell_active),
        ] {
            let base = text_role(style, false, skin);

            assert_eq!(
                text_role(style, true, skin),
                TextRoleSkin { color, ..base },
                "{style:?}"
            );
        }
    }

    #[kithara::test]
    fn a_role_declaring_no_active_colour_ignores_the_flag() {
        let skin = builtin::skin();

        for style in [
            TextStyle::Body,
            TextStyle::Brand,
            TextStyle::TrackTitle,
            TextStyle::Telemetry,
            TextStyle::MicroLabel,
            TextStyle::Section,
            TextStyle::MenuRowStrong,
            TextStyle::MenuHint,
            TextStyle::MenuHintAccent,
            TextStyle::MenuSection,
            TextStyle::MenuCount,
            TextStyle::MenuCaption,
            TextStyle::VisFooter,
            TextStyle::VisMeta,
            TextStyle::VisTitle,
        ] {
            assert_eq!(
                text_role(style, true, skin),
                text_role(style, false, skin),
                "{style:?}"
            );
        }
    }
}
