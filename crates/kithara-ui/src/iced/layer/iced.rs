use std::cell::RefCell;

use iced::{
    Element, Event, Length, Rectangle, Renderer, Size, Theme, Vector,
    advanced::{
        Clipboard, Renderer as _, Shell, Widget as IcedWidget,
        graphics::geometry::Renderer as _,
        layout::{self, Layout, Node},
        mouse, overlay,
        overlay::Nested,
        renderer,
        widget::{Operation, Tree},
    },
    event::Status,
    widget::canvas::Frame,
};

use crate::{
    backends::{finish, replay_ordered},
    draw::Pt,
    hosts::layer::HostLayer,
    iced::tree::window as window_event,
    interact::{CursorShape, Input, Outcome, iced as iced_interact},
    render::{Published, Skin, WindowCommand},
    shaping::TextResources,
};

pub(crate) fn draw_host_layer<A>(
    renderer: &mut Renderer,
    layer: &HostLayer<A>,
    resources: &TextResources,
) {
    let bounds = layer.bounds();
    if bounds.w < 1.0 || bounds.h < 1.0 || layer.draw().commands().is_empty() {
        return;
    }
    renderer.with_translation(Vector::new(bounds.x, bounds.y), |renderer| {
        let mut frame = Frame::new(renderer, Size::new(bounds.w, bounds.h));
        replay_ordered(layer.draw(), &mut frame, resources);
        renderer.draw_geometry(finish(frame.into_geometry()));
    });
}

pub(crate) fn window_layers<'a>(
    child: Element<'a, Published>,
    resize_edges: bool,
    skin: &'a Skin,
) -> Element<'a, Published> {
    Element::new(WindowLayers {
        skin,
        child,
        resize_edges,
    })
}

struct WindowLayers<'a> {
    skin: &'a Skin,
    child: Element<'a, Published>,
    resize_edges: bool,
}

impl WindowLayers<'_> {
    fn child_layout<'a>(layout: Layout<'a>) -> Option<Layout<'a>> {
        layout.children().next()
    }
}

struct WindowOverlay<'a> {
    skin: &'a Skin,
    bounds: Rectangle,
    child: RefCell<Option<Nested<'a, Published, Theme, Renderer>>>,
    resize_edges: bool,
}

impl WindowOverlay<'_> {
    fn draw_layers(&self, renderer: &mut Renderer) {
        if let Some(layer) = self.resize_layer() {
            draw_host_layer(renderer, &layer, self.skin.text_resources.as_ref());
        }
    }

    fn interaction(&self, pointer: mouse::Cursor) -> mouse::Interaction {
        self.resize_layer()
            .map_or(mouse::Interaction::None, |layer| {
                cursor(
                    std::slice::from_ref(&layer),
                    pointer.position().map(Into::into),
                )
                .into()
            })
    }

    fn resize_layer(&self) -> Option<HostLayer<WindowCommand>> {
        self.resize_edges.then(|| {
            crate::hosts::window::surface::frame(self.bounds.into(), self.skin.window.resize_edge)
        })
    }

    fn update_layers(
        &self,
        event: &Event,
        pointer: mouse::Cursor,
        shell: &mut Shell<'_, Published>,
    ) -> bool {
        let input = iced_interact::input(event);
        input.is_some_and(|input| {
            let Some(layer) = self.resize_layer() else {
                return false;
            };
            let outcome = handle(
                std::slice::from_ref(&layer),
                input,
                pointer.position().map(Into::into),
            );
            let Some(command) = outcome.value() else {
                return false;
            };
            if let Some(action) = window_event(command, Outcome::set(())) {
                let (message, redraw, status) = action.into_inner();
                shell.request_redraw_at(redraw);
                if let Some(message) = message {
                    shell.publish(message);
                }
                if status == Status::Captured {
                    shell.capture_event();
                }
            }
            true
        })
    }
}

impl overlay::Overlay<Published, Theme, Renderer> for WindowOverlay<'_> {
    fn draw(
        &self,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        pointer: mouse::Cursor,
    ) {
        if let Some(child_layout) = layout.children().next()
            && let Some(child) = self.child.borrow_mut().as_mut()
        {
            child.draw(renderer, theme, style, child_layout, pointer);
        }
        self.draw_layers(renderer);
    }

    fn layout(&mut self, renderer: &Renderer, bounds: Size) -> Node {
        let children = self
            .child
            .get_mut()
            .as_mut()
            .map(|child| child.layout(renderer, bounds))
            .into_iter()
            .collect();
        Node::with_children(bounds, children)
    }

    fn mouse_interaction(
        &self,
        layout: Layout<'_>,
        pointer: mouse::Cursor,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        let interaction = self.interaction(pointer);
        if interaction != mouse::Interaction::None {
            return interaction;
        }
        let Some(child_layout) = layout.children().next() else {
            return mouse::Interaction::None;
        };
        self.child
            .borrow_mut()
            .as_mut()
            .map_or(mouse::Interaction::None, |child| {
                child.mouse_interaction(child_layout, pointer, renderer)
            })
    }

    fn operate(&mut self, layout: Layout<'_>, renderer: &Renderer, operation: &mut dyn Operation) {
        let Some(child_layout) = layout.children().next() else {
            return;
        };
        if let Some(child) = self.child.get_mut().as_mut() {
            child.operate(child_layout, renderer, operation);
        }
    }

    fn update(
        &mut self,
        event: &Event,
        layout: Layout<'_>,
        pointer: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Published>,
    ) {
        if self.update_layers(event, pointer, shell) {
            return;
        }
        let Some(child_layout) = layout.children().next() else {
            return;
        };
        let Some(child) = self.child.get_mut().as_mut() else {
            return;
        };
        child.update(event, child_layout, pointer, renderer, clipboard, shell);
    }
}

impl IcedWidget<Published, Theme, Renderer> for WindowLayers<'_> {
    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.child)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_ref(&self.child));
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        pointer: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let Some(child_layout) = Self::child_layout(layout) else {
            return;
        };
        self.child.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            child_layout,
            pointer,
            viewport,
        );
    }

    fn layout(&mut self, tree: &mut Tree, renderer: &Renderer, limits: &layout::Limits) -> Node {
        let limits = limits.width(Length::Fill).height(Length::Fill);
        let child = self
            .child
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, &limits);
        let size = limits.resolve(Length::Fill, Length::Fill, child.size());
        Node::with_children(size, vec![child])
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        pointer: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        let Some(child_layout) = Self::child_layout(layout) else {
            return mouse::Interaction::None;
        };
        self.child.as_widget().mouse_interaction(
            &tree.children[0],
            child_layout,
            pointer,
            viewport,
            renderer,
        )
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        let Some(child_layout) = Self::child_layout(layout) else {
            return;
        };
        self.child.as_widget_mut().operate(
            &mut tree.children[0],
            child_layout,
            renderer,
            operation,
        );
    }

    fn overlay<'a>(
        &'a mut self,
        tree: &'a mut Tree,
        layout: Layout<'a>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'a, Published, Theme, Renderer>> {
        let child_layout = Self::child_layout(layout)?;
        let child = self.child.as_widget_mut().overlay(
            &mut tree.children[0],
            child_layout,
            renderer,
            viewport,
            translation,
        );
        if !self.resize_edges {
            return child;
        }
        Some(overlay::Element::new(Box::new(WindowOverlay {
            bounds: layout.bounds(),
            child: RefCell::new(child.map(Nested::new)),
            resize_edges: self.resize_edges,
            skin: self.skin,
        })))
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Fill)
    }

    fn size_hint(&self) -> Size<Length> {
        self.size()
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        pointer: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Published>,
        viewport: &Rectangle,
    ) {
        let Some(child_layout) = Self::child_layout(layout) else {
            return;
        };
        self.child.as_widget_mut().update(
            &mut tree.children[0],
            event,
            child_layout,
            pointer,
            renderer,
            clipboard,
            shell,
            viewport,
        );
    }
}

#[cfg(test)]
mod tests {
    use iced::{
        Pixels, Point,
        advanced::{
            clipboard,
            layout::{Layout, Limits},
            renderer,
            widget::Tree,
        },
        mouse::{self, Button, Cursor},
        widget::{Space, mouse_area},
    };
    use iced_renderer::fallback::Renderer as FallbackRenderer;
    use iced_tiny_skia::Renderer as TinySkiaRenderer;
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        builtin,
        iced::fonts::SANS,
        render::{Published, WindowCommand, WindowEdge},
    };

    fn press_at(x: f32) -> (Vec<Published>, bool) {
        let child = mouse_area(Space::new().width(Length::Fill).height(Length::Fill))
            .on_press(Published::window(WindowCommand::Minimize))
            .into();
        let mut element = window_layers(child, true, builtin::skin());
        let renderer = FallbackRenderer::Secondary(TinySkiaRenderer::new(SANS, Pixels(14.0)));
        let viewport = Size::new(100.0, 60.0);
        let mut tree = Tree::new(element.as_widget());
        let node = element.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &Limits::new(Size::ZERO, viewport),
        );
        let mut clipboard = clipboard::Null;
        let mut messages = Vec::new();
        let mut shell = Shell::new(&mut messages);
        let bounds = Rectangle::with_size(viewport);
        let layout = Layout::new(&node);
        let mut overlay = element
            .as_widget_mut()
            .overlay(&mut tree, layout, &renderer, &bounds, Vector::ZERO)
            .expect("window chrome must produce a root overlay");
        let overlay_node = overlay.as_overlay_mut().layout(&renderer, viewport);
        let event = Event::Mouse(mouse::Event::ButtonPressed(Button::Left));
        let pointer = Cursor::Available(Point::new(x, 30.0));
        overlay.as_overlay_mut().update(
            &event,
            Layout::new(&overlay_node),
            pointer,
            &renderer,
            &mut clipboard,
            &mut shell,
        );
        let layer_captured = shell.is_event_captured();
        drop(overlay);
        if !layer_captured {
            element.as_widget_mut().update(
                &mut tree,
                &event,
                layout,
                pointer,
                &renderer,
                &mut clipboard,
                &mut shell,
                &bounds,
            );
        }
        let captured = shell.is_event_captured();
        drop(shell);
        (messages, captured)
    }

    struct PressOverlayWidget;

    impl IcedWidget<Published, Theme, Renderer> for PressOverlayWidget {
        fn draw(
            &self,
            _tree: &Tree,
            _renderer: &mut Renderer,
            _theme: &Theme,
            _style: &renderer::Style,
            _layout: Layout<'_>,
            _cursor: Cursor,
            _viewport: &Rectangle,
        ) {
        }

        fn layout(&mut self, _tree: &mut Tree, _renderer: &Renderer, limits: &Limits) -> Node {
            Node::new(limits.max())
        }

        fn overlay<'a>(
            &'a mut self,
            _tree: &'a mut Tree,
            _layout: Layout<'a>,
            _renderer: &Renderer,
            _viewport: &Rectangle,
            _translation: Vector,
        ) -> Option<overlay::Element<'a, Published, Theme, Renderer>> {
            Some(overlay::Element::new(Box::new(PressOverlay)))
        }

        fn size(&self) -> Size<Length> {
            Size::new(Length::Fill, Length::Fill)
        }
    }

    struct PressOverlay;

    impl overlay::Overlay<Published, Theme, Renderer> for PressOverlay {
        fn draw(
            &self,
            _renderer: &mut Renderer,
            _theme: &Theme,
            _style: &renderer::Style,
            _layout: Layout<'_>,
            _cursor: Cursor,
        ) {
        }

        fn layout(&mut self, _renderer: &Renderer, bounds: Size) -> Node {
            Node::new(bounds)
        }

        fn mouse_interaction(
            &self,
            layout: Layout<'_>,
            pointer: Cursor,
            _renderer: &Renderer,
        ) -> mouse::Interaction {
            if pointer.is_over(layout.bounds()) {
                mouse::Interaction::Pointer
            } else {
                mouse::Interaction::None
            }
        }

        fn update(
            &mut self,
            event: &Event,
            layout: Layout<'_>,
            pointer: Cursor,
            _renderer: &Renderer,
            _clipboard: &mut dyn Clipboard,
            shell: &mut Shell<'_, Published>,
        ) {
            if matches!(
                event,
                Event::Mouse(mouse::Event::ButtonPressed(Button::Left))
            ) && pointer.is_over(layout.bounds())
            {
                shell.publish(Published::window(WindowCommand::Minimize));
                shell.capture_event();
            }
        }
    }

    fn overlay_press_at(x: f32) -> Vec<Published> {
        let child = Element::new(PressOverlayWidget);
        let mut element = window_layers(child, true, builtin::skin());
        let renderer = FallbackRenderer::Secondary(TinySkiaRenderer::new(SANS, Pixels(14.0)));
        let viewport = Size::new(100.0, 60.0);
        let bounds = Rectangle::with_size(viewport);
        let mut tree = Tree::new(element.as_widget());
        let node = element.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &Limits::new(Size::ZERO, viewport),
        );
        let mut clipboard = clipboard::Null;
        let mut messages = Vec::new();
        let mut shell = Shell::new(&mut messages);
        let mut overlay = element
            .as_widget_mut()
            .overlay(
                &mut tree,
                Layout::new(&node),
                &renderer,
                &bounds,
                Vector::ZERO,
            )
            .expect("window chrome must wrap the child overlay");
        let overlay_node = overlay.as_overlay_mut().layout(&renderer, viewport);
        overlay.as_overlay_mut().update(
            &Event::Mouse(mouse::Event::ButtonPressed(Button::Left)),
            Layout::new(&overlay_node),
            Cursor::Available(Point::new(x, 30.0)),
            &renderer,
            &mut clipboard,
            &mut shell,
        );
        drop(shell);
        messages
    }

    #[kithara::test]
    fn resize_edge_captures_before_the_document_but_one_pixel_inside_reaches_it() {
        let (messages, captured) = press_at(3.0);
        assert_eq!(
            messages,
            [Published::window(WindowCommand::Resize(WindowEdge::West))]
        );
        assert!(captured);

        let (messages, captured) = press_at(5.0);
        assert_eq!(messages, [Published::window(WindowCommand::Minimize)]);
        assert!(captured, "the control underneath owns the inside press");
    }

    #[kithara::test]
    fn resize_edge_is_above_a_document_popup_but_the_inside_press_reaches_it() {
        assert_eq!(
            overlay_press_at(3.0),
            [Published::window(WindowCommand::Resize(WindowEdge::West))]
        );
        assert_eq!(
            overlay_press_at(5.0),
            [Published::window(WindowCommand::Minimize)]
        );
    }

    #[kithara::test]
    fn window_layer_host_fills_the_available_root_when_the_document_shrinks() {
        let child = Space::new()
            .width(Length::Fixed(20.0))
            .height(Length::Fixed(10.0))
            .into();
        let mut element = window_layers(child, true, builtin::skin());
        let renderer = FallbackRenderer::Secondary(TinySkiaRenderer::new(SANS, Pixels(14.0)));
        let viewport = Size::new(100.0, 60.0);
        let mut tree = Tree::new(element.as_widget());
        let node = element.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &Limits::new(Size::ZERO, viewport),
        );

        assert_eq!(node.size(), viewport);
        assert_eq!(node.children()[0].size(), Size::new(20.0, 10.0));
    }
}
pub(crate) fn handle<A: Copy>(
    layers: &[HostLayer<A>],
    input: Input<'_>,
    pointer: Option<Pt>,
) -> Outcome<A> {
    layers
        .iter()
        .rev()
        .map(|layer| layer.handle(input, pointer))
        .find(Outcome::is_captured)
        .unwrap_or(Outcome::IGNORED)
}

pub(crate) fn cursor<A>(layers: &[HostLayer<A>], pointer: Option<Pt>) -> CursorShape {
    layers
        .iter()
        .rev()
        .map(|layer| layer.cursor_at(pointer))
        .find(|shape| *shape != CursorShape::None)
        .unwrap_or(CursorShape::None)
}

#[cfg(test)]
mod model_tests {
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        draw::{DrawList, Pt, Rect},
        hosts::layer::LayerHit,
        interact::{CursorShape, Input, Outcome, PointerPhase, mouse as mouse_input},
    };

    fn pointer_input(phase: PointerPhase, at: Option<Pt>) -> Input<'static> {
        Input::Pointer(mouse_input(phase, at))
    }

    fn rect(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect { h, w, x, y }
    }

    #[kithara::test]
    fn the_last_layer_is_topmost_for_input_and_cursor() {
        let lower = HostLayer::new(
            rect(0.0, 0.0, 100.0, 60.0),
            DrawList::default(),
            vec![LayerHit::new(
                rect(0.0, 0.0, 8.0, 60.0),
                CursorShape::ResizeH,
                1_u8,
            )],
        );
        let upper = HostLayer::new(
            rect(0.0, 0.0, 100.0, 60.0),
            DrawList::default(),
            vec![LayerHit::new(
                rect(0.0, 0.0, 8.0, 60.0),
                CursorShape::Pointer,
                2_u8,
            )],
        );
        let layers = [lower, upper];
        let pointer = Some(Pt { x: 3.0, y: 30.0 });

        assert_eq!(
            handle(&layers, pointer_input(PointerPhase::Down, None), pointer),
            Outcome::set(2)
        );
        assert_eq!(cursor(&layers, pointer), CursorShape::Pointer);
    }

    #[kithara::test]
    fn a_hit_is_half_open_and_a_non_press_is_ignored() {
        let layer = HostLayer::new(
            rect(0.0, 0.0, 100.0, 60.0),
            DrawList::default(),
            vec![LayerHit::new(
                rect(0.0, 0.0, 4.0, 60.0),
                CursorShape::ResizeH,
                7_u8,
            )],
        );
        let layers = [layer];

        assert_eq!(
            handle(
                &layers,
                pointer_input(PointerPhase::Down, None),
                Some(Pt { x: 3.0, y: 30.0 })
            ),
            Outcome::set(7)
        );
        assert_eq!(
            handle(
                &layers,
                pointer_input(PointerPhase::Down, None),
                Some(Pt { x: 4.0, y: 30.0 })
            ),
            Outcome::IGNORED
        );
        assert_eq!(
            handle(
                &layers,
                pointer_input(PointerPhase::Move, Some(Pt { x: 3.0, y: 30.0 })),
                Some(Pt { x: 3.0, y: 30.0 })
            ),
            Outcome::IGNORED
        );
    }
}
