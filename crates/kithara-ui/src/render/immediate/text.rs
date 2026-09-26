use iced::{
    Element, Length, Rectangle, Renderer, Size, Theme,
    advanced::{
        Widget as IcedWidget,
        layout::{self, Layout, Node},
        mouse, renderer,
        widget::{
            Tree,
            tree::{State, Tag},
        },
    },
    widget::Space,
};
use kithara_test_macros as kithara;

use crate::{
    atoms::text::Text as TextAtom,
    draw::{DrawList, DrawListBuilder, Rect, Rgba},
    module::TextStyle,
    render::{
        ReadValue, Skin, UiEvent, Widget,
        controls::{PaintState, Probe},
    },
    skin::{ColorRole, FontFamily, FontWeight, TextRoleSkin},
};

#[derive(bon::Builder)]
pub(crate) struct Text<'value, 'data, 'skin> {
    skin: &'skin Skin,
    active_color: Option<ColorRole>,
    color: Option<ColorRole>,
    font: Option<FontFamily>,
    label: Option<&'data str>,
    value: Option<&'value ReadValue<'data>>,
    weight: Option<FontWeight>,
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
            return Space::new().into();
        };
        let role = self
            .skin
            .text_role(self.style, self.color, self.active_color, self.active)
            .faced(self.font, self.weight);
        let content = self.style.cased(value.to_owned());
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
    skin: &'skin Skin,
    content: String,
    role: TextRoleSkin,
    padding_x: f32,
}

/// Everything a paragraph's picture is a function of: the words, the role they
/// are shaped through, the padding that insets them, the colour the skin gave
/// that role, and the box they were given.
///
/// The colour rather than the skin it came out of, because a paragraph reads
/// exactly one value from the skin and keying on the whole of it would compare
/// a page's worth of resolved style to answer one question.
///
/// The words are a type parameter so that the probe a frame asks with borrows
/// them and only a miss copies them: a paragraph whose words did not move must
/// not copy them to find out that they did not.
#[derive(PartialEq)]
struct TextKey<Content> {
    content: Content,
    bounds: Rect,
    colour: Rgba,
    role: TextRoleSkin,
    padding_x: f32,
}

/// The key a paragraph keeps between frames.
type Words = TextKey<String>;

impl Words {
    /// This kept key seen as a probe, so that both sides of the comparison are
    /// the same type and can derive their equality.
    fn probe(&self) -> TextKey<&str> {
        TextKey {
            bounds: self.bounds,
            colour: self.colour,
            content: &self.content,
            padding_x: self.padding_x,
            role: self.role,
        }
    }
}

impl Probe for TextKey<&str> {
    type Key = Words;

    fn holds(&self, key: &Self::Key) -> bool {
        *self == key.probe()
    }

    fn keep(self) -> Self::Key {
        TextKey {
            bounds: self.bounds,
            colour: self.colour,
            content: self.content.to_owned(),
            padding_x: self.padding_x,
            role: self.role,
        }
    }
}

impl Painted<'_> {
    /// The words this paragraph is about to draw from, kept whole so that the
    /// next frame can ask whether any of them moved.
    fn key(&self, bounds: Rect) -> TextKey<&str> {
        TextKey {
            bounds,
            colour: self.skin.rgba(self.role.color),
            content: &self.content,
            padding_x: self.padding_x,
            role: self.role,
        }
    }

    /// What this paragraph draws in the box it was given.
    fn list(&self, state: &PaintState<Words>, bounds: Rect) -> DrawList {
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
    /// Words are drawn as outlines through the canvas, so tessellating them is
    /// the most expensive thing on a page of prose. The list a paragraph draws
    /// is kept and the geometry behind it reused, exactly as a painted control
    /// does: a page whose words did not change pays for them once.
    #[kithara::measure(label = "iced.text.draw")]
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

        let state = tree.state.downcast_ref::<PaintState<Words>>();
        let local = Rect {
            h: bounds.height,
            w: bounds.width,
            x: 0.0,
            y: 0.0,
        };
        state.mark(self.key(local), || self.list(state, local));
        state.replay(
            renderer,
            bounds,
            |_| Rectangle::with_size(bounds.size()),
            self.skin.text_resources(),
        );
    }

    fn layout(&mut self, tree: &mut Tree, _renderer: &Renderer, limits: &layout::Limits) -> Node {
        let state = tree.state.downcast_mut::<PaintState<Words>>();
        let (width, height) = state.shaped(self.skin.text_resources(), |text| {
            TextAtom::new(&self.content, self.role, self.padding_x, self.skin).measure(text)
        });
        Node::new(limits.resolve(Length::Shrink, Length::Fill, Size::new(width, height)))
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Shrink, Length::Fill)
    }

    fn state(&self) -> State {
        State::new(PaintState::<Words>::default())
    }

    fn tag(&self) -> Tag {
        Tag::of::<PaintState<Words>>()
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
    use crate::{builtin, render::Marked};

    mod consts {
        use super::*;

        pub(super) const BOX: Rect = Rect {
            h: 20.0,
            w: 160.0,
            x: 0.0,
            y: 0.0,
        };
    }

    /// One frame of the immediate-mode host: the paragraph is built afresh, and
    /// the canvas state is the one thing that survived the last frame.
    fn frame(state: &PaintState<Words>, content: &str, skin: &Skin) -> Marked {
        let painted = Painted {
            content: content.to_owned(),
            padding_x: 0.0,
            role: skin.text_role(TextStyle::Body, None, None, false),
            skin,
        };
        state.mark(painted.key(consts::BOX), || {
            painted.list(state, consts::BOX)
        })
    }

    /// Shaping a paragraph and tessellating its outlines is the most expensive
    /// thing on a page of prose. The host rebuilds the widget every frame, so a
    /// paragraph whose words did not move must not be drawn from again at all.
    #[kithara::test]
    fn unchanged_words_are_not_drawn_again() {
        let skin = builtin::skin();
        let state = PaintState::default();

        assert_eq!(
            frame(&state, "ZVUK", skin),
            Marked::Changed,
            "the first frame draws"
        );
        assert_eq!(
            frame(&state, "ZVUK", skin),
            Marked::Kept,
            "words that did not change must cost nothing to draw"
        );
    }

    /// The other half of the same contract: what was drawn must not outlive the
    /// words it was built from.
    #[kithara::test]
    fn changed_words_draw_again() {
        let skin = builtin::skin();
        let state = PaintState::default();

        frame(&state, "ZVUK", skin);

        assert_eq!(
            frame(&state, "LOCAL", skin),
            Marked::Changed,
            "a paragraph must not be left showing the words it no longer says"
        );
    }
}
