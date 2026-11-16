use iced::{
    Element, Event, Length, Point, Rectangle, Renderer, Size, Theme, Vector,
    advanced::{
        Clipboard, Renderer as _, Shell, Widget as IcedWidget,
        graphics::geometry::Renderer as _,
        layout::{Layout, Limits, Node},
        mouse,
        mouse::Interaction,
        overlay, renderer,
        widget::{
            Operation, Tree,
            tree::{State, Tag},
        },
    },
    widget::canvas::Frame,
};

use crate::{
    backends::{finish, replay_ordered},
    draw::{DrawListBuilder, Rect},
    hosts::scroll::{Bar, Window},
    interact::iced as iced_interact,
    render::{Published, Skin},
};

/// A window over a child taller than itself.
///
/// This host had been borrowing the toolkit's own scrollable, which keeps its
/// offset where nothing else can read it: an indicator beside it would have
/// been a second copy of the same number. A viewport of this toolkit's own
/// keeps the one window both hosts keep, so the wheel means the same distance
/// on either and the bar comes out of the same three numbers.
pub(crate) struct Viewport<'a> {
    skin: &'a Skin,
    child: Element<'a, Published>,
    height: Length,
    width: Length,
}

impl<'a> Viewport<'a> {
    pub(crate) const fn new(
        child: Element<'a, Published>,
        width: Length,
        height: Length,
        skin: &'a Skin,
    ) -> Self {
        Self {
            skin,
            child,
            height,
            width,
        }
    }
}

impl IcedWidget<Published, Theme, Renderer> for Viewport<'_> {
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
        renderer.with_layer(bounds, |renderer| {
            renderer.with_translation(Vector::new(bounds.x, bounds.y), |renderer| {
                let mut frame = Frame::new(renderer, bounds.size());
                replay_ordered(&list, &mut frame, self.skin.text_resources.as_ref());
                renderer.draw_geometry(finish(frame.into_geometry()));
            });
        });
    }

    /// The window is the declared box; the child keeps whatever height it asked
    /// for and the window moves it under itself.
    ///
    /// The scrolled axis is measured uncompressed, which is what makes a `Fill`
    /// child report the content it has rather than claim the window: a
    /// compressed limit would make the content exactly as tall as the window,
    /// and there would be nothing to scroll to.
    fn layout(&mut self, tree: &mut Tree, renderer: &Renderer, limits: &Limits) -> Node {
        let size = limits.resolve(self.width, self.height, limits.max());
        let inner = Limits::with_compression(
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
        Node::with_children(size, vec![child])
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _viewport: &Rectangle,
        renderer: &Renderer,
    ) -> Interaction {
        let bounds = layout.bounds();
        layout
            .children()
            .next()
            .map_or_else(Interaction::default, |child| {
                self.child.as_widget().mouse_interaction(
                    &tree.children[0],
                    child,
                    cursor,
                    &bounds,
                    renderer,
                )
            })
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

    fn overlay<'a>(
        &'a mut self,
        tree: &'a mut Tree,
        layout: Layout<'a>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'a, Published, Theme, Renderer>> {
        overlay::from_children(
            std::slice::from_mut(&mut self.child),
            tree,
            layout,
            renderer,
            viewport,
            translation,
        )
    }

    fn size(&self) -> Size<Length> {
        Size::new(self.width, self.height)
    }

    fn state(&self) -> State {
        State::new(Window::new())
    }

    fn tag(&self) -> Tag {
        Tag::of::<Window>()
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
        shell: &mut Shell<'_, Published>,
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
}

impl<'a> From<Viewport<'a>> for Element<'a, Published> {
    fn from(viewport: Viewport<'a>) -> Self {
        Self::new(viewport)
    }
}
