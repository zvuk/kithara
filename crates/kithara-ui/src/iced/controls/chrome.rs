use std::cell::RefCell;

use iced::{
    Element, Event, Length, Rectangle, Renderer, Size, Theme, Vector,
    advanced::{
        Renderer as _, Widget as IcedWidget,
        graphics::geometry::Renderer as _,
        layout::{self, Layout, Node},
        renderer,
        widget::{
            Tree,
            tree::{State, Tag},
        },
    },
    mouse::{Cursor, Interaction},
    widget::canvas::{self, Action, Canvas, Frame, Geometry},
};
use kithara_platform::sync::Arc;

use crate::{
    atoms::chrome::{ChromeChevron, ChromeLabel, footer_role},
    backends::{finish, replay_ordered},
    draw::{DrawListBuilder, Rect},
    iced::{
        controls::{Marks, painted::shaped_with_resources, snapped},
        paint::text::Text,
        tree::activate,
    },
    interact::{CursorShape, Hover, iced as iced_interact, recognizers::click},
    render::{InputOwner, Published, Skin},
    shaping::{TextContext, TextResources},
};

/// The footer carries the one word a module resolves for itself, so it owns it
/// where the rest of the chrome borrows what the document already interned.
#[derive(Clone)]
pub(crate) enum ChromeLeaf<'a> {
    Chip(&'a str),
    Title(&'a str),
    Footer(String),
    HorizontalLine,
    VerticalLine,
}

pub(crate) fn chrome_leaf<'a>(leaf: ChromeLeaf<'a>, skin: &'a Skin) -> Element<'a, Published> {
    Element::new(LeafPaint { skin, leaf })
}

struct LeafPaint<'data, 'skin> {
    skin: &'skin Skin,
    leaf: ChromeLeaf<'data>,
}

#[derive(Default)]
struct LeafState {
    marks: Marks<()>,
    text: RefCell<Option<(Arc<TextResources>, TextContext)>>,
}

impl LeafPaint<'_, '_> {
    /// The label this leaf draws, and what it says. A line has neither, and a
    /// footer is a word with no box around it.
    fn label(&self) -> Option<(ChromeLabel, &str)> {
        match &self.leaf {
            ChromeLeaf::Chip(label) => Some((ChromeLabel::chip(self.skin), label)),
            ChromeLeaf::Title(title) => Some((ChromeLabel::title(self.skin), title)),
            ChromeLeaf::Footer(_) | ChromeLeaf::HorizontalLine | ChromeLeaf::VerticalLine => None,
        }
    }

    fn lengths(&self) -> Size<Length> {
        match &self.leaf {
            ChromeLeaf::Chip(_) | ChromeLeaf::Title(_) => Size::new(Length::Shrink, Length::Fill),
            ChromeLeaf::Footer(_) => Size::new(Length::Fill, Length::Fill),
            ChromeLeaf::HorizontalLine => Size::new(
                Length::Fill,
                Length::Fixed(self.skin.chrome.inner_line_width),
            ),
            ChromeLeaf::VerticalLine => Size::new(
                Length::Fixed(self.skin.chrome.inner_line_width),
                Length::Fill,
            ),
        }
    }

    fn paint(&self, builder: &mut DrawListBuilder, text: &mut TextContext, bounds: Rect) {
        if let Some((label, content)) = self.label() {
            label.paint(builder, text, content, bounds);
            return;
        }
        match &self.leaf {
            ChromeLeaf::Footer(content) => Text::new(
                content,
                footer_role(self.skin),
                self.skin.chrome.footer_pad,
                self.skin,
            )
            .paint(builder, text, bounds),
            _ => builder.fill_rect(bounds, self.skin.rgba(self.skin.chrome.inner_line)),
        }
    }
}

impl IcedWidget<Published, Theme, Renderer> for LeafPaint<'_, '_> {
    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        _theme: &Theme,
        _style: &renderer::Style,
        layout: Layout<'_>,
        _cursor: Cursor,
        _viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();
        if bounds.width < 1.0 || bounds.height < 1.0 {
            return;
        }
        let bounds = snapped(bounds);
        let state = tree.state.downcast_ref::<LeafState>();
        let resources = &self.skin.text_resources;
        let list = shaped_with_resources(&state.text, resources, |text| {
            let mut builder = DrawListBuilder::default();
            self.paint(
                &mut builder,
                text,
                Rect {
                    h: bounds.height,
                    w: bounds.width,
                    x: 0.0,
                    y: 0.0,
                },
            );
            builder.finish()
        });
        state.marks.picture(list);
        state.marks.drawn(|cache, list| {
            renderer.with_translation(Vector::new(bounds.x, bounds.y), |renderer| {
                let geometry = cache.draw(renderer, bounds.size(), |frame| {
                    replay_ordered(list, frame, resources.as_ref());
                });
                renderer.draw_geometry(geometry);
            });
        });
    }

    fn layout(&mut self, tree: &mut Tree, _renderer: &Renderer, limits: &layout::Limits) -> Node {
        let intrinsic = if let Some((label, content)) = self.label() {
            let state = tree.state.downcast_mut::<LeafState>();
            let (width, height) =
                shaped_with_resources(&state.text, &self.skin.text_resources, |text| {
                    label.intrinsic(text, content)
                });
            Size::new(width, height)
        } else {
            Size::ZERO
        };
        let lengths = self.lengths();
        Node::new(limits.resolve(lengths.width, lengths.height, intrinsic))
    }

    fn size(&self) -> Size<Length> {
        self.lengths()
    }

    fn state(&self) -> State {
        State::new(LeafState::default())
    }

    fn tag(&self) -> Tag {
        Tag::of::<LeafState>()
    }
}

pub(crate) fn header_chevron<'a>(
    path: String,
    collapsed: bool,
    skin: &'a Skin,
    owner: InputOwner,
) -> Element<'a, Published> {
    let paint = ChevronPaint {
        chevron: ChromeChevron::new(skin),
        collapsed,
        skin,
    };
    match owner {
        InputOwner::Leaf => Canvas::new(ChevronProgram { paint, path })
            .width(Length::Fill)
            .height(Length::Fill)
            .into(),
        InputOwner::Engine => Canvas::new(paint)
            .width(Length::Fill)
            .height(Length::Fill)
            .into(),
    }
}

struct ChevronProgram<'skin> {
    paint: ChevronPaint<'skin>,
    path: String,
}

impl canvas::Program<Published> for ChevronProgram<'_> {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: Cursor,
    ) -> Vec<Geometry> {
        self.paint.geometry(renderer, bounds)
    }

    fn mouse_interaction(&self, _state: &(), bounds: Rectangle, cursor: Cursor) -> Interaction {
        Hover::new(CursorShape::Pointer)
            .cursor(false, &iced_interact::hit(bounds, cursor))
            .into()
    }

    fn update(
        &self,
        _state: &mut (),
        event: &Event,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> Option<Action<Published>> {
        let input = iced_interact::input(event)?;
        let hit = iced_interact::hit(bounds, cursor);
        activate(&self.path, click::on_input(input, &hit))
    }
}

/// The chevron drawn over the header, through the same neutral mark the
/// retained host draws in its own cell.
struct ChevronPaint<'skin> {
    skin: &'skin Skin,
    chevron: ChromeChevron,
    collapsed: bool,
}

impl ChevronPaint<'_> {
    fn geometry(&self, renderer: &Renderer, bounds: Rectangle) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let mut list = DrawListBuilder::default();
        self.chevron.paint(
            &mut list,
            Rect {
                h: bounds.height,
                w: bounds.width,
                x: 0.0,
                y: 0.0,
            },
            self.collapsed,
        );
        replay_ordered(
            &list.finish(),
            &mut frame,
            self.skin.text_resources.as_ref(),
        );
        vec![finish(frame.into_geometry())]
    }
}

impl canvas::Program<Published> for ChevronPaint<'_> {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: Cursor,
    ) -> Vec<Geometry> {
        self.geometry(renderer, bounds)
    }
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use iced::{
        Pixels, Point,
        advanced::{graphics::text::font_system, layout::Limits, widget::Tree},
        alignment::Vertical,
        event, mouse,
        widget::container,
        window::RedrawRequest,
    };
    use iced_renderer::fallback::Renderer as FallbackRenderer;
    use iced_tiny_skia::Renderer as TinySkiaRenderer;
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        builtin,
        draw::DrawCmd,
        iced::{controls::painted::picture_tests, fonts, skin::IcedSkin},
        render::ControlAction,
        shaping::FontId,
        skin::FontWeight,
    };

    /// The word iced lays out on its own, against which the painted leaf is
    /// measured: shaped the way every string this toolkit sets is shaped.
    fn shaped(content: &'static str) -> iced::widget::Text<'static> {
        iced::widget::Text::new(content).shaping(iced::widget::text::Shaping::Advanced)
    }

    fn headless_renderer() -> Renderer {
        let mut system = font_system()
            .write()
            .unwrap_or_else(|error| panic!("iced font system must be available: {error}"));
        for bytes in fonts::FONT_BYTES {
            system.load_font(Cow::Borrowed(bytes));
        }
        drop(system);

        FallbackRenderer::Secondary(TinySkiaRenderer::new(fonts::SANS, Pixels(14.0)))
    }

    fn measured_size(mut element: Element<'_, Published>, renderer: &Renderer) -> Size {
        let mut tree = Tree::new(element.as_widget());
        element
            .as_widget_mut()
            .layout(
                &mut tree,
                renderer,
                &Limits::new(Size::ZERO, Size::new(320.0, 80.0)),
            )
            .size()
    }

    #[kithara::test]
    fn chrome_leaves_paint_through_the_retained_builder() {
        let skin = builtin::skin();
        let leaf = LeafPaint {
            skin,
            leaf: ChromeLeaf::Chip("FX"),
        };
        let mut builder = DrawListBuilder::default();
        let mut text = TextContext::from(skin.text_resources.as_ref());
        leaf.paint(
            &mut builder,
            &mut text,
            Rect {
                h: skin.chrome.header_height,
                w: 40.0,
                x: 0.0,
                y: 0.0,
            },
        );
        let list = builder.finish();

        assert!(matches!(
            list.commands(),
            [DrawCmd::Fill { .. }, DrawCmd::Text { content, .. }] if content == "FX"
        ));

        let line = LeafPaint {
            skin,
            leaf: ChromeLeaf::HorizontalLine,
        };
        let mut builder = DrawListBuilder::default();
        line.paint(
            &mut builder,
            &mut text,
            Rect {
                h: skin.chrome.inner_line_width,
                w: 80.0,
                x: 0.0,
                y: 0.0,
            },
        );
        assert!(matches!(
            builder.finish().commands(),
            [DrawCmd::Fill { .. }]
        ));

        let title = LeafPaint {
            skin,
            leaf: ChromeLeaf::Title("DECK"),
        };
        let mut builder = DrawListBuilder::default();
        title.paint(
            &mut builder,
            &mut text,
            Rect {
                h: skin.chrome.header_height,
                w: 80.0,
                x: 0.0,
                y: 0.0,
            },
        );
        assert!(matches!(
            builder.finish().commands(),
            [DrawCmd::Fill { .. }, DrawCmd::Text { content, .. }] if content == "DECK"
        ));

        let line = LeafPaint {
            skin,
            leaf: ChromeLeaf::VerticalLine,
        };
        let mut builder = DrawListBuilder::default();
        line.paint(
            &mut builder,
            &mut text,
            Rect {
                h: skin.chrome.header_height,
                w: skin.chrome.inner_line_width,
                x: 0.0,
                y: 0.0,
            },
        );
        assert!(matches!(
            builder.finish().commands(),
            [DrawCmd::Fill { .. }]
        ));
    }

    #[kithara::test]
    fn painted_header_text_keeps_the_iced_intrinsic_size() {
        let skin = builtin::skin();
        let metrics = skin.chrome;
        let renderer = headless_renderer();
        let chip: Element<'_, Published> = container(
            shaped("FX")
                .font(fonts::mono(FontWeight::Normal))
                .size(metrics.chip_text.size)
                .color(skin.color(metrics.chip_text.color)),
        )
        .padding([0.0, metrics.chip_pad])
        .height(Length::Fill)
        .align_y(Vertical::Center)
        .into();
        let title: Element<'_, Published> = container(
            shaped("DECK")
                .font(fonts::display(FontWeight::Medium))
                .size(metrics.title_text.size)
                .color(skin.color(metrics.title_text.color)),
        )
        .padding([0.0, metrics.chip_pad])
        .height(Length::Fill)
        .align_y(Vertical::Center)
        .into();

        for (old, painted) in [
            (chip, chrome_leaf(ChromeLeaf::Chip("FX"), skin)),
            (title, chrome_leaf(ChromeLeaf::Title("DECK"), skin)),
        ] {
            let old = measured_size(old, &renderer);
            let painted = measured_size(painted, &renderer);
            assert!((painted.width - old.width).abs() < 0.001);
            assert!((painted.height - old.height).abs() < 0.001);
        }
    }

    #[kithara::test]
    fn the_leaf_header_canvas_publishes_a_press_on_the_header() {
        let program = ChevronProgram {
            path: "app-deck/header".to_owned(),
            paint: ChevronPaint {
                chevron: ChromeChevron::new(builtin::skin()),
                collapsed: false,
                skin: builtin::skin(),
            },
        };
        let bounds = Rectangle {
            height: 28.0,
            width: 240.0,
            x: 0.0,
            y: 0.0,
        };
        let cursor = Cursor::Available(Point::new(20.0, 14.0));
        let press = Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left));

        let action = canvas::Program::update(&program, &mut (), &press, bounds, cursor)
            .expect("a press anywhere in the header must toggle its module");

        assert_eq!(
            action.into_inner(),
            (
                Some(Published::Gesture {
                    path: "app-deck/header".to_owned(),
                    action: ControlAction::Activate,
                }),
                RedrawRequest::Wait,
                event::Status::Captured,
            )
        );
    }

    fn draw_leaf(
        leaf: &LeafPaint<'_, '_>,
        tree: &Tree,
        renderer: &mut Renderer,
        bounds: Rectangle,
    ) -> iced_tiny_skia::Layer {
        picture_tests::reset(renderer);
        let node = Node::new(bounds.size()).move_to(Point::new(bounds.x, bounds.y));
        leaf.draw(
            tree,
            renderer,
            &Theme::Dark,
            &renderer::Style {
                text_color: iced::Color::WHITE,
            },
            Layout::new(&node),
            Cursor::Unavailable,
            &Rectangle::with_size(Size::new(640.0, 480.0)),
        );
        picture_tests::layer(renderer)
    }

    fn leaf_intrinsic(leaf: &mut LeafPaint<'_, '_>, tree: &mut Tree, renderer: &Renderer) -> Size {
        leaf.layout(
            tree,
            renderer,
            &Limits::new(Size::ZERO, Size::new(320.0, 80.0)),
        )
        .size()
    }

    #[kithara::test]
    fn leaf_redraws_reuse_backing_and_changed_content_matches_fresh() {
        let skin = picture_tests::skin(FontId::InterRegular);
        let mut renderer = picture_tests::renderer();
        let leaf = LeafPaint {
            skin: &skin,
            leaf: ChromeLeaf::Chip(picture_tests::WORDS),
        };
        let tree = Tree::new(&leaf as &dyn IcedWidget<Published, Theme, Renderer>);
        let bounds = Rectangle {
            x: 9.0,
            y: 13.0,
            width: 220.0,
            height: 40.0,
        };
        let first = draw_leaf(&leaf, &tree, &mut renderer, bounds);
        let second = draw_leaf(&leaf, &tree, &mut renderer, bounds);
        let third = draw_leaf(&leaf, &tree, &mut renderer, bounds);
        picture_tests::assert_reused(&first, &second);
        picture_tests::assert_reused(&second, &third);
        picture_tests::assert_picture(&first, &second);
        picture_tests::assert_picture(&second, &third);

        let translated = Rectangle {
            x: 31.0,
            y: 47.0,
            ..bounds
        };
        let moved = draw_leaf(&leaf, &tree, &mut renderer, translated);
        picture_tests::assert_reused(&third, &moved);
        assert_eq!(
            picture_tests::primitives(&third),
            picture_tests::primitives(&moved)
        );
        for (old, new) in third.primitives.iter().zip(&moved.primitives) {
            assert_eq!(
                old.transformation().translation(),
                Vector::new(bounds.x, bounds.y)
            );
            assert_eq!(
                new.transformation().translation(),
                Vector::new(translated.x, translated.y)
            );
            assert_eq!(old.clip_bounds(), bounds);
            assert_eq!(new.clip_bounds(), translated);
            assert_eq!(
                old.clip_bounds() * old.transformation().inverse(),
                Rectangle::with_size(bounds.size()),
            );
            assert_eq!(
                new.clip_bounds() * new.transformation().inverse(),
                Rectangle::with_size(translated.size()),
            );
        }
        let fresh_moved = draw_leaf(
            &leaf,
            &Tree::new(&leaf as &dyn IcedWidget<Published, Theme, Renderer>),
            &mut renderer,
            translated,
        );
        picture_tests::assert_picture(&moved, &fresh_moved);

        let changed = LeafPaint {
            skin: &skin,
            leaf: ChromeLeaf::Chip("Changed visible word"),
        };
        let redrawn = draw_leaf(&changed, &tree, &mut renderer, translated);
        let fresh = draw_leaf(
            &changed,
            &Tree::new(&changed as &dyn IcedWidget<Published, Theme, Renderer>),
            &mut renderer,
            translated,
        );
        picture_tests::assert_changed(&moved, &redrawn);
        picture_tests::assert_picture(&redrawn, &fresh);
    }

    #[kithara::test]
    fn leaf_font_owner_replacement_matches_fresh_measurement_and_paint() {
        let a = picture_tests::skin(FontId::InterRegular);
        let b = picture_tests::skin(FontId::JetBrainsMonoRegular);
        assert_ne!(
            FontId::InterRegular.bytes(),
            FontId::JetBrainsMonoRegular.bytes()
        );
        assert_eq!(a.chrome, b.chrome);
        picture_tests::assert_font(&a, a.chrome.chip_text, FontId::InterRegular);
        picture_tests::assert_font(&b, b.chrome.chip_text, FontId::JetBrainsMonoRegular);
        let mut renderer = picture_tests::renderer();
        let mut first = LeafPaint {
            skin: &a,
            leaf: ChromeLeaf::Chip(picture_tests::WORDS),
        };
        let mut retained = Tree::new(&first as &dyn IcedWidget<Published, Theme, Renderer>);
        let size_a = leaf_intrinsic(&mut first, &mut retained, &renderer);
        let bounds = Rectangle::with_size(Size::new(220.0, 40.0));
        let drawn_a = draw_leaf(&first, &retained, &mut renderer, bounds);

        let mut changed = LeafPaint {
            skin: &b,
            leaf: ChromeLeaf::Chip(picture_tests::WORDS),
        };
        let retained_size = leaf_intrinsic(&mut changed, &mut retained, &renderer);
        let mut fresh_tree = Tree::new(&changed as &dyn IcedWidget<Published, Theme, Renderer>);
        let fresh_size = leaf_intrinsic(&mut changed, &mut fresh_tree, &renderer);
        assert_ne!(
            size_a, fresh_size,
            "the fixture must exercise different font metrics"
        );
        assert_eq!(retained_size, fresh_size);
        let retained_b = draw_leaf(&changed, &retained, &mut renderer, bounds);
        let fresh_b = draw_leaf(&changed, &fresh_tree, &mut renderer, bounds);
        picture_tests::assert_changed(&drawn_a, &retained_b);
        picture_tests::assert_picture(&retained_b, &fresh_b);
        let again = draw_leaf(&changed, &retained, &mut renderer, bounds);
        picture_tests::assert_reused(&retained_b, &again);
        picture_tests::assert_picture(&again, &fresh_b);
    }
}
