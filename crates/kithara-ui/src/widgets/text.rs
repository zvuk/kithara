use std::cell::RefCell;

use iced::{
    Element, Length, Rectangle, Renderer, Size, Theme, Vector,
    advanced::{
        Renderer as _, Widget as IcedWidget,
        graphics::geometry::Renderer as _,
        layout::{self, Layout},
        mouse, renderer,
        widget::{self, Tree},
    },
    widget::canvas::Frame,
};

use crate::{
    atoms::text::Text as TextAtom,
    backends::replay_ordered,
    draw::{DrawListBuilder, Rect},
    module::TextStyle,
    render::{ReadValue, Skin, UiEvent, tree::active_tone},
    skin::{ColorRole, TextRoleSkin},
    text::TextContext,
    widgets::Widget,
};

#[derive(bon::Builder)]
pub(crate) struct Text<'value, 'data, 'skin> {
    skin: &'skin Skin,
    active_color: Option<ColorRole>,
    color: Option<ColorRole>,
    label: Option<&'data str>,
    value: Option<&'value ReadValue<'data>>,
    style: TextStyle,
    active: bool,
}

impl<'a, 'value, 'data, 'skin> Widget<'a> for Text<'value, 'data, 'skin>
where
    'skin: 'a,
{
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
        Painted {
            content,
            padding_x,
            role,
            skin: self.skin,
        }
        .into()
    }
}

struct Painted<'skin> {
    content: String,
    padding_x: f32,
    role: TextRoleSkin,
    skin: &'skin Skin,
}

#[derive(Default)]
struct State {
    text: RefCell<Option<TextContext>>,
}

impl IcedWidget<UiEvent, Theme, Renderer> for Painted<'_> {
    fn size(&self) -> Size<Length> {
        Size::new(Length::Shrink, Length::Fill)
    }

    fn tag(&self) -> widget::tree::Tag {
        widget::tree::Tag::of::<State>()
    }

    fn state(&self) -> widget::tree::State {
        widget::tree::State::new(State::default())
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        _renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let state = tree.state.downcast_mut::<State>();
        let mut text = state.text.borrow_mut();
        let text = text.get_or_insert_with(|| self.skin.text_resources().into());
        let (width, height) =
            TextAtom::new(&self.content, self.role, self.padding_x, self.skin).measure(text);
        layout::Node::new(limits.resolve(Length::Shrink, Length::Fill, Size::new(width, height)))
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        _theme: &Theme,
        _style: &renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        _viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();

        if bounds.width < 1.0 || bounds.height < 1.0 {
            return;
        }

        let state = tree.state.downcast_ref::<State>();
        let mut text = state.text.borrow_mut();
        let text = text.get_or_insert_with(|| self.skin.text_resources().into());

        renderer.with_translation(Vector::new(bounds.x, bounds.y), |renderer| {
            let mut frame = Frame::new(renderer, bounds.size());
            let mut builder = DrawListBuilder::default();
            TextAtom::new(&self.content, self.role, self.padding_x, self.skin).paint(
                &mut builder,
                text,
                Rect {
                    h: bounds.height,
                    w: bounds.width,
                    x: 0.0,
                    y: 0.0,
                },
            );
            replay_ordered(&builder.finish(), &mut frame, self.skin.text_resources());
            renderer.draw_geometry(frame.into_geometry());
        });
    }
}

impl<'a> From<Painted<'a>> for Element<'a, UiEvent> {
    fn from(painted: Painted<'a>) -> Self {
        Self::new(painted)
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
        TextStyle::Mono => (skin.text.mono, None),
        TextStyle::Caption => (skin.text.caption, None),
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
            (TextStyle::Mono, skin.text.mono),
            (TextStyle::Caption, skin.text.caption),
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
            text_role(TextStyle::Mono, Some(ColorRole::Text), None, false, skin),
            TextRoleSkin {
                color: ColorRole::Text,
                ..skin.text.mono
            }
        );
    }

    #[kithara::test]
    fn a_node_switches_between_the_two_colours_it_names() {
        let skin = builtin::skin();
        let role = |active| {
            text_role(
                TextStyle::Mono,
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
                ..skin.text.mono
            }
        );
        assert_eq!(
            role(false),
            TextRoleSkin {
                color: ColorRole::Muted,
                ..skin.text.mono
            }
        );
    }

    #[kithara::test]
    fn an_active_node_naming_one_colour_keeps_it() {
        let skin = builtin::skin();

        assert_eq!(
            text_role(
                TextStyle::Caption,
                Some(ColorRole::Accent),
                None,
                true,
                skin
            ),
            TextRoleSkin {
                color: ColorRole::Accent,
                ..skin.text.caption
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
    fn brand_small_resolves_under_the_display_family_and_never_the_mono_one() {
        let skin = builtin::skin();
        let role = text_role(TextStyle::BrandSmall, None, None, false, skin);

        assert_eq!(role, skin.text.brand_small);
        assert_eq!(
            text_role(TextStyle::BrandSmall, None, None, true, skin),
            role
        );
        assert_ne!(
            role.font, skin.text.mono.font,
            "the mono micro roles are Mono and the brand pair is Display"
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
            TextStyle::Mono,
            TextStyle::Caption,
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
