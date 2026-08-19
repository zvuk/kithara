use std::cell::RefCell;

use iced::{
    Element, Event, Length, Rectangle, Renderer, Size, Theme, Vector,
    advanced::{
        Clipboard, Renderer as _, Shell, Widget as IcedWidget,
        graphics::geometry::Renderer as _,
        layout::{self, Layout},
        mouse, overlay, renderer,
        widget::{self, Operation, Tree},
    },
    widget::canvas::Frame,
};

use super::{cursor, handle};
use crate::{
    backends::replay_ordered,
    interact::{Input, Outcome, PointerPhase, iced as iced_interact},
    render::{
        DragGhost, HostLayer, Skin, UiEvent, WindowCommand, WindowSurface, window as window_event,
    },
    shaping::{TextContext, TextResources},
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
        renderer.draw_geometry(frame.into_geometry());
    });
}

pub(crate) fn window_layers<'a>(
    child: Element<'a, UiEvent>,
    dragged: Option<String>,
    resize_edges: bool,
    skin: &'a Skin,
) -> Element<'a, UiEvent> {
    Element::new(WindowLayers {
        child,
        ghost: dragged.map(|label| DragGhost::new(Some(label), skin)),
        resize_edges,
        skin,
    })
}

struct WindowLayers<'a> {
    child: Element<'a, UiEvent>,
    ghost: Option<DragGhost>,
    resize_edges: bool,
    skin: &'a Skin,
}

struct LayerState {
    text: RefCell<Option<TextContext>>,
}

impl WindowLayers<'_> {
    fn child_layout<'a>(layout: Layout<'a>) -> Option<Layout<'a>> {
        layout.children().next()
    }
}

struct WindowOverlay<'a> {
    bounds: Rectangle,
    child: RefCell<Option<overlay::Nested<'a, UiEvent, Theme, Renderer>>>,
    ghost: Option<&'a DragGhost>,
    resize_edges: bool,
    skin: &'a Skin,
    text: &'a RefCell<Option<TextContext>>,
}

impl WindowOverlay<'_> {
    fn resize_layer(&self) -> Option<HostLayer<WindowCommand>> {
        self.resize_edges
            .then(|| WindowSurface::frame(self.bounds.into(), self.skin.window.resize_edge))
    }

    fn update_layers(
        &self,
        event: &Event,
        pointer: mouse::Cursor,
        shell: &mut Shell<'_, UiEvent>,
    ) -> bool {
        let input = iced_interact::input(event);
        if self.ghost.is_some()
            && matches!(
                input,
                Some(Input::Pointer(pointer)) if pointer.phase == PointerPhase::Move
            )
        {
            shell.request_redraw();
        }
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
                if status == iced::event::Status::Captured {
                    shell.capture_event();
                }
            }
            true
        })
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

    fn draw_layers(&self, renderer: &mut Renderer, pointer: mouse::Cursor) {
        if let Some(layer) = self.resize_layer() {
            draw_host_layer(renderer, &layer, self.skin.text_resources());
        }
        if let Some(ghost) = self.ghost {
            let mut text = self.text.borrow_mut();
            let text = text.get_or_insert_with(|| self.skin.text_resources().into());
            let layer = ghost.layer(pointer.position().map(Into::into), self.bounds.into(), text);
            draw_host_layer(renderer, &layer, self.skin.text_resources());
        }
    }
}

impl overlay::Overlay<UiEvent, Theme, Renderer> for WindowOverlay<'_> {
    fn layout(&mut self, renderer: &Renderer, bounds: Size) -> layout::Node {
        let children = self
            .child
            .get_mut()
            .as_mut()
            .map(|child| child.layout(renderer, bounds))
            .into_iter()
            .collect();
        layout::Node::with_children(bounds, children)
    }

    fn update(
        &mut self,
        event: &Event,
        layout: Layout<'_>,
        pointer: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, UiEvent>,
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
        self.draw_layers(renderer, pointer);
    }

    fn operate(&mut self, layout: Layout<'_>, renderer: &Renderer, operation: &mut dyn Operation) {
        let Some(child_layout) = layout.children().next() else {
            return;
        };
        if let Some(child) = self.child.get_mut().as_mut() {
            child.operate(child_layout, renderer, operation);
        }
    }
}

impl IcedWidget<UiEvent, Theme, Renderer> for WindowLayers<'_> {
    fn tag(&self) -> widget::tree::Tag {
        widget::tree::Tag::of::<LayerState>()
    }

    fn state(&self) -> widget::tree::State {
        widget::tree::State::new(LayerState {
            text: RefCell::new(None),
        })
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.child)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_ref(&self.child));
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Fill)
    }

    fn size_hint(&self) -> Size<Length> {
        self.size()
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let limits = limits.width(Length::Fill).height(Length::Fill);
        let child = self
            .child
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, &limits);
        let size = limits.resolve(Length::Fill, Length::Fill, child.size());
        layout::Node::with_children(size, vec![child])
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        pointer: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, UiEvent>,
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
    ) -> Option<overlay::Element<'a, UiEvent, Theme, Renderer>> {
        let child_layout = Self::child_layout(layout)?;
        let child = self.child.as_widget_mut().overlay(
            &mut tree.children[0],
            child_layout,
            renderer,
            viewport,
            translation,
        );
        if !self.resize_edges && self.ghost.is_none() {
            return child;
        }
        let state = tree.state.downcast_ref::<LayerState>();
        Some(overlay::Element::new(Box::new(WindowOverlay {
            bounds: layout.bounds(),
            child: RefCell::new(child.map(overlay::Nested::new)),
            ghost: self.ghost.as_ref(),
            resize_edges: self.resize_edges,
            skin: self.skin,
            text: &state.text,
        })))
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
        window,
    };
    use iced_renderer::fallback::Renderer as FallbackRenderer;
    use iced_tiny_skia::Renderer as TinySkiaRenderer;
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        builtin,
        render::{UiEvent, WindowCommand, WindowEdge, fonts::SANS},
    };

    fn press_at(x: f32) -> (Vec<UiEvent>, bool) {
        let child = mouse_area(Space::new().width(Length::Fill).height(Length::Fill))
            .on_press(UiEvent::OpenSettings)
            .into();
        let mut element = window_layers(child, None, true, builtin::skin());
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
            .unwrap_or_else(|| panic!("window chrome must produce a root overlay"));
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

    impl IcedWidget<UiEvent, Theme, Renderer> for PressOverlayWidget {
        fn size(&self) -> Size<Length> {
            Size::new(Length::Fill, Length::Fill)
        }

        fn layout(
            &mut self,
            _tree: &mut Tree,
            _renderer: &Renderer,
            limits: &Limits,
        ) -> layout::Node {
            layout::Node::new(limits.max())
        }

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

        fn overlay<'a>(
            &'a mut self,
            _tree: &'a mut Tree,
            _layout: Layout<'a>,
            _renderer: &Renderer,
            _viewport: &Rectangle,
            _translation: Vector,
        ) -> Option<overlay::Element<'a, UiEvent, Theme, Renderer>> {
            Some(overlay::Element::new(Box::new(PressOverlay)))
        }
    }

    struct PressOverlay;

    impl overlay::Overlay<UiEvent, Theme, Renderer> for PressOverlay {
        fn layout(&mut self, _renderer: &Renderer, bounds: Size) -> layout::Node {
            layout::Node::new(bounds)
        }

        fn update(
            &mut self,
            event: &Event,
            layout: Layout<'_>,
            pointer: Cursor,
            _renderer: &Renderer,
            _clipboard: &mut dyn Clipboard,
            shell: &mut Shell<'_, UiEvent>,
        ) {
            if matches!(
                event,
                Event::Mouse(mouse::Event::ButtonPressed(Button::Left))
            ) && pointer.is_over(layout.bounds())
            {
                shell.publish(UiEvent::OpenSettings);
                shell.capture_event();
            }
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

        fn draw(
            &self,
            _renderer: &mut Renderer,
            _theme: &Theme,
            _style: &renderer::Style,
            _layout: Layout<'_>,
            _cursor: Cursor,
        ) {
        }
    }

    fn overlay_press_at(x: f32) -> Vec<UiEvent> {
        let child = Element::new(PressOverlayWidget);
        let mut element = window_layers(child, None, true, builtin::skin());
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
            .unwrap_or_else(|| panic!("window chrome must wrap the child overlay"));
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
            [UiEvent::Window(WindowCommand::Resize(WindowEdge::West))]
        );
        assert!(captured);

        let (messages, captured) = press_at(5.0);
        assert_eq!(messages, [UiEvent::OpenSettings]);
        assert!(captured, "the control underneath owns the inside press");
    }

    #[kithara::test]
    fn resize_edge_is_above_a_document_popup_but_the_inside_press_reaches_it() {
        assert_eq!(
            overlay_press_at(3.0),
            [UiEvent::Window(WindowCommand::Resize(WindowEdge::West))]
        );
        assert_eq!(overlay_press_at(5.0), [UiEvent::OpenSettings]);
    }

    #[kithara::test]
    fn window_layer_host_fills_the_available_root_when_the_document_shrinks() {
        let child = Space::new()
            .width(Length::Fixed(20.0))
            .height(Length::Fixed(10.0))
            .into();
        let mut element = window_layers(child, None, true, builtin::skin());
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

    #[kithara::test]
    fn moving_the_drag_ghost_redraws_without_publishing_or_capturing() {
        let child = Space::new().width(Length::Fill).height(Length::Fill).into();
        let mut element = window_layers(
            child,
            Some("Signal Path".to_owned()),
            false,
            builtin::skin(),
        );
        let renderer = FallbackRenderer::Secondary(TinySkiaRenderer::new(SANS, Pixels(14.0)));
        let viewport = Size::new(240.0, 80.0);
        let mut tree = Tree::new(element.as_widget());
        let node = element.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &Limits::new(Size::ZERO, viewport),
        );
        let mut clipboard = clipboard::Null;
        let mut messages = Vec::new();
        let mut shell = Shell::new(&mut messages);
        let pointer = Point::new(30.0, 30.0);
        let bounds = Rectangle::with_size(viewport);
        let mut overlay = element
            .as_widget_mut()
            .overlay(
                &mut tree,
                Layout::new(&node),
                &renderer,
                &bounds,
                Vector::ZERO,
            )
            .unwrap_or_else(|| panic!("the drag ghost must produce a root overlay"));
        let overlay_node = overlay.as_overlay_mut().layout(&renderer, viewport);
        overlay.as_overlay_mut().update(
            &Event::Mouse(mouse::Event::CursorMoved { position: pointer }),
            Layout::new(&overlay_node),
            Cursor::Available(pointer),
            &renderer,
            &mut clipboard,
            &mut shell,
        );

        let captured = shell.is_event_captured();
        let redraw = shell.redraw_request();
        drop(shell);

        assert!(messages.is_empty());
        assert!(!captured);
        assert_ne!(redraw, window::RedrawRequest::Wait);
    }
}
