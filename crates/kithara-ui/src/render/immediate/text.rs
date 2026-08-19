use iced::{
    Element, Length, Rectangle, Renderer, Size, Theme,
    advanced::{
        Widget as IcedWidget,
        layout::{self, Layout},
        mouse, renderer,
        widget::{self, Tree},
    },
};

use crate::{
    atoms::text::Text as TextAtom,
    draw::{DrawList, DrawListBuilder, Rect},
    module::TextStyle,
    render::{ReadValue, Skin, UiEvent, Widget, controls::PaintState},
    skin::{ColorRole, TextRoleSkin},
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
        let role = self
            .skin
            .text_role(self.style, self.color, self.active_color, self.active);
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

impl Painted<'_> {
    /// What this paragraph draws in the box it was given.
    fn list(&self, state: &PaintState, bounds: Rect) -> DrawList {
        state.shaped(self.skin.text_resources(), |text| {
            let mut builder = DrawListBuilder::default();
            TextAtom::new(&self.content, self.role, self.padding_x, self.skin).paint(
                &mut builder,
                text,
                bounds,
            );
            builder.finish()
        })
    }
}

impl IcedWidget<UiEvent, Theme, Renderer> for Painted<'_> {
    fn size(&self) -> Size<Length> {
        Size::new(Length::Shrink, Length::Fill)
    }

    fn tag(&self) -> widget::tree::Tag {
        widget::tree::Tag::of::<PaintState>()
    }

    fn state(&self) -> widget::tree::State {
        widget::tree::State::new(PaintState::default())
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        _renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let state = tree.state.downcast_mut::<PaintState>();
        let (width, height) = state.shaped(self.skin.text_resources(), |text| {
            TextAtom::new(&self.content, self.role, self.padding_x, self.skin).measure(text)
        });
        layout::Node::new(limits.resolve(Length::Shrink, Length::Fill, Size::new(width, height)))
    }

    /// Words are drawn as outlines through the canvas, so tessellating them is
    /// the most expensive thing on a page of prose. The list a paragraph draws
    /// is kept and the geometry behind it reused, exactly as a painted control
    /// does: a page whose words did not change pays for them once.
    #[cfg_attr(feature = "perf", hotpath::measure(label = "iced.text.draw"))]
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

        let state = tree.state.downcast_ref::<PaintState>();
        let list = self.list(
            state,
            Rect {
                h: bounds.height,
                w: bounds.width,
                x: 0.0,
                y: 0.0,
            },
        );
        state.replay(
            renderer,
            bounds,
            Rectangle::with_size(bounds.size()),
            &list,
            self.skin.text_resources(),
        );
    }
}

impl<'a> From<Painted<'a>> for Element<'a, UiEvent> {
    fn from(painted: Painted<'a>) -> Self {
        Self::new(painted)
    }
}

/// What a paragraph is allowed to keep between frames.
#[cfg(test)]
mod cached {
    use kithara_test_utils::kithara;

    use super::*;
    use crate::builtin;

    const BOX: Rect = Rect {
        h: 20.0,
        w: 160.0,
        x: 0.0,
        y: 0.0,
    };

    /// One frame of the immediate-mode host: the paragraph is built afresh, and
    /// the canvas state is the one thing that survived the last frame.
    fn frame(state: &PaintState, content: &str, skin: &Skin) -> bool {
        let painted = Painted {
            content: content.to_owned(),
            padding_x: 0.0,
            role: skin.text_role(TextStyle::Body, None, None, false),
            skin,
        };
        state.refresh(&painted.list(state, BOX))
    }

    /// Glyph outlines are the most expensive thing on a page of prose to
    /// tessellate. The host rebuilds every paragraph each frame, so a paragraph
    /// whose words did not change must keep the geometry it drew.
    #[kithara::test]
    fn unchanged_words_keep_the_glyphs_they_drew() {
        let skin = builtin::skin();
        let state = PaintState::default();

        assert!(frame(&state, "ZVUK", skin), "the first frame draws");
        assert!(
            !frame(&state, "ZVUK", skin),
            "words that did not change must keep what they drew"
        );
    }

    /// The other half of the same contract: kept geometry must not outlive the
    /// words it was built from.
    #[kithara::test]
    fn changed_words_draw_again() {
        let skin = builtin::skin();
        let state = PaintState::default();

        assert!(frame(&state, "ZVUK", skin));
        assert!(
            frame(&state, "LOCAL", skin),
            "a paragraph must not be left showing the words it no longer says"
        );
    }
}
