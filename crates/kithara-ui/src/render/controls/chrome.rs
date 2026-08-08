use std::cell::RefCell;

use iced::{
    Element, Event, Length, Point, Rectangle, Renderer, Size, Theme, Vector,
    advanced::{
        Renderer as _, Widget as IcedWidget,
        graphics::geometry::Renderer as _,
        layout::{self, Layout},
        renderer,
        widget::{self, Tree},
    },
    mouse::{Cursor, Interaction},
    widget::canvas::{self, Action, Canvas, Frame, Geometry, Path, Stroke},
};

use crate::{
    atoms::text::Text,
    backends::replay_ordered,
    draw::{DrawListBuilder, Rect},
    interact::{CursorShape, Hover, iced as iced_interact, recognizers::click},
    render::{IcedSkin, InputOwner, Skin, UiEvent, toggle_module},
    skin::{ColorRole, FontFamily, FontWeight, FrameSkin, TextRoleSkin},
    text::TextContext,
};

#[derive(Clone, Copy)]
pub(crate) enum ChromeLeaf<'a> {
    Chip(&'a str),
    Title(&'a str),
    HorizontalLine,
    VerticalLine,
}

pub(crate) fn chrome_leaf<'a>(leaf: ChromeLeaf<'a>, skin: &'a Skin) -> Element<'a, UiEvent> {
    Element::new(LeafPaint { leaf, skin })
}

struct LeafPaint<'data, 'skin> {
    leaf: ChromeLeaf<'data>,
    skin: &'skin Skin,
}

#[derive(Default)]
struct LeafState {
    text: RefCell<Option<TextContext>>,
}

impl LeafPaint<'_, '_> {
    fn lengths(&self) -> Size<Length> {
        match self.leaf {
            ChromeLeaf::Chip(_) | ChromeLeaf::Title(_) => Size::new(Length::Shrink, Length::Fill),
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

    fn text(&self) -> Option<Text<'_, '_>> {
        let metrics = self.skin.chrome;
        match self.leaf {
            ChromeLeaf::Chip(label) => Some(Text::new(
                label,
                TextRoleSkin {
                    color: metrics.chip_text,
                    font: FontFamily::Mono,
                    size: metrics.chip_text_size,
                    spacing: 0.0,
                    weight: FontWeight::Normal,
                },
                metrics.chip_pad,
                self.skin,
            )),
            ChromeLeaf::Title(title) => Some(Text::new(
                title,
                TextRoleSkin {
                    color: metrics.title_text,
                    font: FontFamily::Display,
                    size: metrics.title_text_size,
                    spacing: 0.0,
                    weight: FontWeight::Medium,
                },
                metrics.chip_pad,
                self.skin,
            )),
            ChromeLeaf::HorizontalLine | ChromeLeaf::VerticalLine => None,
        }
    }

    fn frame(&self) -> Option<(FrameSkin, ColorRole)> {
        match self.leaf {
            ChromeLeaf::Chip(_) => Some((
                self.skin.chrome.chip_frame,
                self.skin.chrome.chip_background,
            )),
            ChromeLeaf::Title(_) => Some((
                self.skin.chrome.title_frame,
                self.skin.chrome.title_background,
            )),
            ChromeLeaf::HorizontalLine | ChromeLeaf::VerticalLine => None,
        }
    }

    fn paint(&self, builder: &mut DrawListBuilder, text: &mut TextContext, bounds: Rect) {
        if let Some((frame, background)) = self.frame() {
            builder.fill_rounded_rect(bounds, frame.radius, self.skin.rgba(background));
            if frame.border_width > 0.0 {
                let inset = frame.border_width / 2.0;
                builder.stroke_rounded_rect(
                    Rect {
                        h: (bounds.h - frame.border_width).max(0.0),
                        w: (bounds.w - frame.border_width).max(0.0),
                        x: bounds.x + inset,
                        y: bounds.y + inset,
                    },
                    frame.radius,
                    self.skin.rgba(frame.border),
                    frame.border_width,
                );
            }
            if let Some(label) = self.text() {
                label.paint(builder, text, bounds);
            }
        } else {
            builder.fill_rect(bounds, self.skin.rgba(self.skin.chrome.inner_line));
        }
    }
}

impl IcedWidget<UiEvent, Theme, Renderer> for LeafPaint<'_, '_> {
    fn size(&self) -> Size<Length> {
        self.lengths()
    }

    fn tag(&self) -> widget::tree::Tag {
        widget::tree::Tag::of::<LeafState>()
    }

    fn state(&self) -> widget::tree::State {
        widget::tree::State::new(LeafState::default())
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        _renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let intrinsic = if let Some(label) = self.text() {
            let state = tree.state.downcast_mut::<LeafState>();
            let mut text = state.text.borrow_mut();
            let text = text.get_or_insert_with(|| self.skin.text_resources().into());
            let (width, height) = label.measure(text);
            Size::new(width, height)
        } else {
            Size::ZERO
        };
        let lengths = self.lengths();
        layout::Node::new(limits.resolve(lengths.width, lengths.height, intrinsic))
    }

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
        let state = tree.state.downcast_ref::<LeafState>();
        let mut text = state.text.borrow_mut();
        let text = text.get_or_insert_with(|| self.skin.text_resources().into());
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
            replay_ordered(&builder.finish(), &mut frame, self.skin.text_resources());
            renderer.draw_geometry(frame.into_geometry());
        });
    }
}

pub(crate) fn header_chevron<'a>(
    module: &str,
    collapsed: bool,
    skin: &'a Skin,
    owner: InputOwner,
) -> Element<'a, UiEvent> {
    let paint = ChevronPaint::new(collapsed, skin);
    match owner {
        InputOwner::Leaf => Canvas::new(ChevronProgram {
            module: module.to_owned(),
            paint,
        })
        .width(Length::Fill)
        .height(Length::Fill)
        .into(),
        InputOwner::Engine => Canvas::new(paint)
            .width(Length::Fill)
            .height(Length::Fill)
            .into(),
    }
}

struct ChevronProgram {
    module: String,
    paint: ChevronPaint,
}

impl canvas::Program<UiEvent> for ChevronProgram {
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
    ) -> Option<Action<UiEvent>> {
        let input = iced_interact::input(event)?;
        let hit = iced_interact::hit(bounds, cursor);
        toggle_module(&self.module, click::on_input(input, &hit))
    }
}

struct ChevronPaint {
    cell_width: f32,
    collapsed: bool,
    color: iced::Color,
    icon_size: f32,
    line_color: iced::Color,
    line_width: f32,
    stroke_width: f32,
}

impl ChevronPaint {
    fn new(collapsed: bool, skin: &Skin) -> Self {
        Self {
            cell_width: skin.chrome.chevron_size,
            collapsed,
            color: skin.color(skin.chrome.chevron_color),
            icon_size: skin.chrome.chevron_icon_size,
            line_color: skin.color(skin.chrome.inner_line),
            line_width: skin.chrome.inner_line_width,
            stroke_width: skin.chrome.chevron_stroke_width,
        }
    }

    fn geometry(&self, renderer: &Renderer, bounds: Rectangle) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let cell_x = (bounds.width - self.cell_width).max(0.0);
        frame.fill_rectangle(
            Point::new(cell_x, 0.0),
            Size::new(self.line_width, bounds.height),
            self.line_color,
        );
        let center = Point::new(cell_x + self.cell_width / 2.0, bounds.height / 2.0);
        let half = self.icon_size / 2.0;
        let rise = self.icon_size / 4.0;
        let direction = if self.collapsed { 1.0 } else { -1.0 };
        let path = Path::new(|builder| {
            builder.move_to(Point::new(center.x - half, center.y - rise * direction));
            builder.line_to(Point::new(center.x, center.y + rise * direction));
            builder.line_to(Point::new(center.x + half, center.y - rise * direction));
        });
        frame.stroke(
            &path,
            Stroke::default()
                .with_color(self.color)
                .with_width(self.stroke_width),
        );
        vec![frame.into_geometry()]
    }
}

impl canvas::Program<UiEvent> for ChevronPaint {
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
        render::{fonts, shaped_text},
    };

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

    fn measured_size(mut element: Element<'_, UiEvent>, renderer: &Renderer) -> Size {
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
            leaf: ChromeLeaf::Chip("FX"),
            skin,
        };
        let mut builder = DrawListBuilder::default();
        let mut text = TextContext::from(skin.text_resources());
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
            leaf: ChromeLeaf::HorizontalLine,
            skin,
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
            leaf: ChromeLeaf::Title("DECK"),
            skin,
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
            leaf: ChromeLeaf::VerticalLine,
            skin,
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
        let chip: Element<'_, UiEvent> = container(
            shaped_text("FX")
                .font(fonts::MONO)
                .size(metrics.chip_text_size)
                .color(skin.color(metrics.chip_text)),
        )
        .padding([0.0, metrics.chip_pad])
        .height(Length::Fill)
        .align_y(Vertical::Center)
        .into();
        let title: Element<'_, UiEvent> = container(
            shaped_text("DECK")
                .font(fonts::display(FontWeight::Medium))
                .size(metrics.title_text_size)
                .color(skin.color(metrics.title_text)),
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
    fn the_leaf_header_canvas_publishes_the_module_toggle() {
        let program = ChevronProgram {
            module: "studio-deck".to_owned(),
            paint: ChevronPaint::new(false, builtin::skin()),
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
            .unwrap_or_else(|| panic!("a press anywhere in the header must toggle its module"));

        assert_eq!(
            action.into_inner(),
            (
                Some(UiEvent::ToggleModule("studio-deck".to_owned())),
                RedrawRequest::Wait,
                event::Status::Captured,
            )
        );
    }
}
