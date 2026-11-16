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

use crate::{
    atoms::chrome::{ChromeChevron, ChromeLabel, footer_role},
    backends::{finish, replay_ordered},
    draw::{DrawListBuilder, Rect},
    iced::{controls::snapped, paint::text::Text, tree::activate},
    interact::{CursorShape, Hover, iced as iced_interact, recognizers::click},
    render::{InputOwner, Published, Skin},
    shaping::TextContext,
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
    text: RefCell<Option<TextContext>>,
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
        let mut text = state.text.borrow_mut();
        let text = text.get_or_insert_with(|| self.skin.text_resources.as_ref().into());
        renderer.with_translation(Vector::new(bounds.x, bounds.y), |renderer| {
            let mut frame = Frame::new(renderer, bounds.size());
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
            replay_ordered(
                &builder.finish(),
                &mut frame,
                self.skin.text_resources.as_ref(),
            );
            renderer.draw_geometry(finish(frame.into_geometry()));
        });
    }

    fn layout(&mut self, tree: &mut Tree, _renderer: &Renderer, limits: &layout::Limits) -> Node {
        let intrinsic = if let Some((label, content)) = self.label() {
            let state = tree.state.downcast_mut::<LeafState>();
            let mut text = state.text.borrow_mut();
            let text = text.get_or_insert_with(|| self.skin.text_resources.as_ref().into());
            let (width, height) = label.intrinsic(text, content);
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
        iced::{fonts, skin::IcedSkin},
        render::ControlAction,
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
}
