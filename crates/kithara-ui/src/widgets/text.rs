use iced::{Element, Length, alignment::Vertical, widget::container};

use crate::{
    module::TextStyle,
    render::{ReadValue, Skin, UiEvent, tree::active_tone, typography::styled_text},
    skin::{ColorRole, TextRoleSkin},
    widgets::Widget,
};

#[derive(bon::Builder)]
pub(crate) struct Text<'value, 'data, 'skin> {
    style: TextStyle,
    value: Option<&'value ReadValue<'data>>,
    label: Option<&'data str>,
    color: Option<ColorRole>,
    active_color: Option<ColorRole>,
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
        let role = text_role(
            self.style,
            self.color,
            self.active_color,
            self.active,
            self.skin,
        );
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

fn text_role(
    style: TextStyle,
    color: Option<ColorRole>,
    active_color: Option<ColorRole>,
    active: bool,
    skin: &Skin,
) -> TextRoleSkin {
    let (role, skin_active) = match style {
        TextStyle::Body => (skin.text.body, None),
        TextStyle::Brand => (skin.text.brand, None),
        TextStyle::BrandSmall => (skin.text.brand_small, None),
        TextStyle::DeckLetter => (skin.text.deck_letter, Some(skin.text.deck_letter_active)),
        TextStyle::TrackTitle => (skin.text.track_title, None),
        TextStyle::Telemetry => (skin.text.telemetry, None),
        TextStyle::MicroLabel => (skin.text.micro_label, None),
        TextStyle::Section => (skin.text.section, None),
        TextStyle::MenuRow => (skin.menu.row, None),
        TextStyle::MenuHint => (skin.menu.hint, None),
        TextStyle::MenuSection => (skin.menu.section, None),
        TextStyle::MenuCount => (skin.menu.count, None),
        TextStyle::MenuCaption => (skin.menu.caption, None),
        TextStyle::VisFooter | TextStyle::VisMeta => (skin.vis.meta, None),
        TextStyle::VisTitle => (skin.vis.title, None),
    };
    TextRoleSkin {
        color: active_tone(color, active_color.or(skin_active), active).unwrap_or(role.color),
        ..role
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{builtin, skin::ColorRole};

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
            (TextStyle::MenuHint, skin.menu.hint),
            (TextStyle::MenuSection, skin.menu.section),
            (TextStyle::MenuCount, skin.menu.count),
            (TextStyle::MenuCaption, skin.menu.caption),
            (TextStyle::VisFooter, skin.vis.meta),
            (TextStyle::VisMeta, skin.vis.meta),
            (TextStyle::VisTitle, skin.vis.title),
        ] {
            assert_eq!(text_role(style, None, None, false, skin), role, "{style:?}");
        }
    }

    #[kithara::test]
    fn a_node_colour_stands_in_for_the_one_the_role_carries() {
        let skin = builtin::skin();

        assert_eq!(
            text_role(TextStyle::MenuRow, Some(ColorRole::Text), None, false, skin),
            TextRoleSkin {
                color: ColorRole::Text,
                ..skin.menu.row
            }
        );
    }

    #[kithara::test]
    fn a_node_switches_between_the_two_colours_it_names() {
        let skin = builtin::skin();
        let role = |active| {
            text_role(
                TextStyle::MenuRow,
                Some(ColorRole::Muted),
                Some(ColorRole::Accent),
                active,
                skin,
            )
        };

        assert_eq!(
            role(true),
            TextRoleSkin {
                color: ColorRole::Accent,
                ..skin.menu.row
            }
        );
        assert_eq!(
            role(false),
            TextRoleSkin {
                color: ColorRole::Muted,
                ..skin.menu.row
            }
        );
    }

    #[kithara::test]
    fn an_active_node_naming_one_colour_keeps_it() {
        let skin = builtin::skin();

        assert_eq!(
            text_role(
                TextStyle::MenuHint,
                Some(ColorRole::Accent),
                None,
                true,
                skin
            ),
            TextRoleSkin {
                color: ColorRole::Accent,
                ..skin.menu.hint
            }
        );
    }

    #[kithara::test]
    fn the_deck_letter_takes_the_active_colour_its_skin_entry_declares() {
        let skin = builtin::skin();
        let base = text_role(TextStyle::DeckLetter, None, None, false, skin);

        assert_eq!(base, skin.text.deck_letter);
        assert_eq!(
            text_role(TextStyle::DeckLetter, None, None, true, skin),
            TextRoleSkin {
                color: skin.text.deck_letter_active,
                ..base
            }
        );
        assert_eq!(
            text_role(
                TextStyle::DeckLetter,
                None,
                Some(ColorRole::Warning),
                true,
                skin
            ),
            TextRoleSkin {
                color: ColorRole::Warning,
                ..base
            }
        );
    }

    #[kithara::test]
    fn brand_small_resolves_under_text_and_never_under_menu() {
        let skin = builtin::skin();
        let role = text_role(TextStyle::BrandSmall, None, None, false, skin);

        assert_eq!(role, skin.text.brand_small);
        assert_eq!(
            text_role(TextStyle::BrandSmall, None, None, true, skin),
            role
        );
        assert_ne!(
            role.font, skin.menu.row.font,
            "the menu family is Mono and the brand pair is Display"
        );
    }

    #[kithara::test]
    fn a_style_declaring_no_active_colour_ignores_the_flag() {
        let skin = builtin::skin();

        for style in [
            TextStyle::Body,
            TextStyle::Brand,
            TextStyle::TrackTitle,
            TextStyle::Telemetry,
            TextStyle::MicroLabel,
            TextStyle::Section,
            TextStyle::MenuRow,
            TextStyle::MenuHint,
            TextStyle::MenuSection,
            TextStyle::MenuCount,
            TextStyle::MenuCaption,
            TextStyle::VisFooter,
            TextStyle::VisMeta,
            TextStyle::VisTitle,
        ] {
            assert_eq!(
                text_role(style, None, None, true, skin),
                text_role(style, None, None, false, skin),
                "{style:?}"
            );
        }
    }
}
