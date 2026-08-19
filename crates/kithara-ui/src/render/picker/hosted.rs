use std::cell::RefCell;

use iced::{
    Event, Renderer, Size, Theme,
    advanced::{
        Clipboard, Shell,
        layout::{self, Layout},
        mouse, overlay, renderer,
        widget::Operation,
    },
};

use crate::render::UiEvent;

pub(crate) fn hosted_picker_overlay<'a>(
    child: overlay::Element<'a, UiEvent, Theme, Renderer>,
    route: impl for<'b> FnMut(&Event, mouse::Cursor, &mut Shell<'b, UiEvent>) -> bool + 'a,
) -> overlay::Element<'a, UiEvent, Theme, Renderer> {
    overlay::Element::new(Box::new(HostedPickerPortal {
        child: RefCell::new(overlay::Nested::new(child)),
        route,
    }))
}

struct HostedPickerPortal<'a, F> {
    child: RefCell<overlay::Nested<'a, UiEvent, Theme, Renderer>>,
    route: F,
}

impl<F> overlay::Overlay<UiEvent, Theme, Renderer> for HostedPickerPortal<'_, F>
where
    F: for<'a> FnMut(&Event, mouse::Cursor, &mut Shell<'a, UiEvent>) -> bool,
{
    fn layout(&mut self, _renderer: &Renderer, _bounds: Size) -> layout::Node {
        layout::Node::new(Size::ZERO)
    }

    fn draw(
        &self,
        _renderer: &mut Renderer,
        _theme: &Theme,
        _style: &renderer::Style,
        _layout: Layout<'_>,
        _cursor: mouse::Cursor,
    ) {
    }

    fn overlay<'a>(
        &'a mut self,
        _layout: Layout<'a>,
        _renderer: &Renderer,
    ) -> Option<overlay::Element<'a, UiEvent, Theme, Renderer>> {
        Some(overlay::Element::new(Box::new(HostedPickerLayer {
            child: &self.child,
            route: &mut self.route,
        })))
    }
}

struct HostedPickerLayer<'a, 'child, F> {
    child: &'a RefCell<overlay::Nested<'child, UiEvent, Theme, Renderer>>,
    route: &'a mut F,
}

impl<F> overlay::Overlay<UiEvent, Theme, Renderer> for HostedPickerLayer<'_, '_, F>
where
    F: for<'a> FnMut(&Event, mouse::Cursor, &mut Shell<'a, UiEvent>) -> bool,
{
    delegate::delegate! {
        to self.child.borrow_mut() {
            fn layout(&mut self, renderer: &Renderer, bounds: Size) -> layout::Node;
            fn mouse_interaction(
                &self,
                layout: Layout<'_>,
                cursor: mouse::Cursor,
                renderer: &Renderer,
            ) -> mouse::Interaction;
            fn draw(
                &self,
                renderer: &mut Renderer,
                theme: &Theme,
                style: &renderer::Style,
                layout: Layout<'_>,
                cursor: mouse::Cursor,
            );
            fn operate(&mut self, layout: Layout<'_>, renderer: &Renderer, operation: &mut dyn Operation);
        }
    }

    fn update(
        &mut self,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, UiEvent>,
    ) {
        if (self.route)(event, cursor, shell) {
            return;
        }
        self.child
            .borrow_mut()
            .update(event, layout, cursor, renderer, clipboard, shell);
    }
}
