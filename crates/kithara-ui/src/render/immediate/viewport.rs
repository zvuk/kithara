use iced::{
    Element, Event, Length, Point, Rectangle, Renderer, Size, Theme, Vector,
    advanced::{
        Clipboard, Renderer as _, Shell, Widget as IcedWidget,
        graphics::geometry::Renderer as _,
        layout::{self, Layout},
        mouse, overlay, renderer,
        widget::{self, Operation, Tree},
    },
    widget::canvas::Frame,
};

use crate::{
    backends::replay_ordered,
    draw::{DrawListBuilder, Rect},
    interact::iced as iced_interact,
    render::{
        Skin, UiEvent,
        scroll::{Bar, Window},
    },
};

/// A window over a child taller than itself.
///
/// This host had been borrowing the toolkit's own scrollable, which keeps its
/// offset where nothing else can read it: an indicator beside it would have
/// been a second copy of the same number. A viewport of this toolkit's own
/// keeps the one window both hosts keep, so the wheel means the same distance
/// on either and the bar comes out of the same three numbers.
pub(crate) struct Viewport<'a> {
    child: Element<'a, UiEvent>,
    height: Length,
    skin: &'a Skin,
    width: Length,
}

impl<'a> Viewport<'a> {
    pub(crate) const fn new(
        child: Element<'a, UiEvent>,
        width: Length,
        height: Length,
        skin: &'a Skin,
    ) -> Self {
        Self {
            child,
            height,
            skin,
            width,
        }
    }
}

impl IcedWidget<UiEvent, Theme, Renderer> for Viewport<'_> {
    fn size(&self) -> Size<Length> {
        Size::new(self.width, self.height)
    }

    fn tag(&self) -> widget::tree::Tag {
        widget::tree::Tag::of::<Window>()
    }

    fn state(&self) -> widget::tree::State {
        widget::tree::State::new(Window::new())
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.child)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_ref(&self.child));
    }

    /// The window is the declared box; the child keeps whatever height it asked
    /// for and the window moves it under itself.
    ///
    /// The scrolled axis is measured uncompressed, which is what makes a `Fill`
    /// child report the content it has rather than claim the window: a
    /// compressed limit would make the content exactly as tall as the window,
    /// and there would be nothing to scroll to.
    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let size = limits.resolve(self.width, self.height, limits.max());
        let inner = layout::Limits::with_compression(
            Size::ZERO,
            Size::new(size.width, f32::INFINITY),
            Size::new(false, true),
        );
        let mut child = self
            .child
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, &inner);
        let offset = tree
            .state
            .downcast_mut::<Window>()
            .measured(child.size().height, size.height);
        child.move_to_mut(Point::new(0.0, -offset));
        layout::Node::with_children(size, vec![child])
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();
        let Some(child) = layout.children().next() else {
            return;
        };
        renderer.with_layer(bounds, |renderer| {
            self.child.as_widget().draw(
                &tree.children[0],
                renderer,
                theme,
                style,
                child,
                cursor,
                &bounds,
            );
        });
        let mut list = DrawListBuilder::default();
        tree.state.downcast_ref::<Window>().indicate(
            Rect {
                h: bounds.height,
                w: bounds.width,
                x: 0.0,
                y: 0.0,
            },
            Bar::new(self.skin),
            &mut list,
        );
        let list = list.finish();
        if list.commands().is_empty() {
            return;
        }
        // Its own layer, pushed after the child's: within one layer this
        // toolkit orders by primitive kind rather than by call, so an
        // indicator drawn beside the child ends up under whichever row has a
        // background — which is how the bar came out broken exactly where the
        // navigator's selected item sits.
        renderer.with_layer(bounds, |renderer| {
            renderer.with_translation(Vector::new(bounds.x, bounds.y), |renderer| {
                let mut frame = Frame::new(renderer, bounds.size());
                replay_ordered(&list, &mut frame, self.skin.text_resources());
                renderer.draw_geometry(frame.into_geometry());
            });
        });
    }

    /// A wheel over the window is the window's own while it still has somewhere
    /// to go; at either end of the travel it is left for whatever encloses it.
    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, UiEvent>,
        viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();
        if cursor.is_over(bounds)
            && let Some(input) = iced_interact::input(event)
            && tree.state.downcast_mut::<Window>().wheel(input)
        {
            shell.capture_event();
            shell.invalidate_layout();
            shell.request_redraw();
            return;
        }
        let Some(child) = layout.children().next() else {
            return;
        };
        self.child.as_widget_mut().update(
            &mut tree.children[0],
            event,
            child,
            cursor,
            renderer,
            clipboard,
            shell,
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
        let bounds = layout.bounds();
        let Some(child) = layout.children().next() else {
            return;
        };
        operation.container(None, bounds);
        operation.traverse(&mut |operation| {
            self.child
                .as_widget_mut()
                .operate(&mut tree.children[0], child, renderer, operation);
        });
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        let bounds = layout.bounds();
        layout
            .children()
            .next()
            .map_or_else(mouse::Interaction::default, |child| {
                self.child.as_widget().mouse_interaction(
                    &tree.children[0],
                    child,
                    cursor,
                    &bounds,
                    renderer,
                )
            })
    }

    fn overlay<'a>(
        &'a mut self,
        tree: &'a mut Tree,
        layout: Layout<'a>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'a, UiEvent, Theme, Renderer>> {
        overlay::from_children(
            std::slice::from_mut(&mut self.child),
            tree,
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}

impl<'a> From<Viewport<'a>> for Element<'a, UiEvent> {
    fn from(viewport: Viewport<'a>) -> Self {
        Self::new(viewport)
    }
}
