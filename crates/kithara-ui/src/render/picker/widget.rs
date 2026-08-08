use std::{any::Any, cell::RefCell, rc::Rc};

use iced::{
    Element, Event, Length, Rectangle, Renderer, Size, Theme, Vector,
    advanced::{
        Clipboard, Shell, Widget as IcedWidget,
        layout::{self, Layout},
        mouse, overlay, renderer,
        widget::{self, Operation, Tree},
    },
};
use kithara_platform::time::Instant;

use super::{
    overlay::PickerPortal,
    paint::{PickerPaint, picker_selected_index},
    program::targets,
};
use crate::{
    draw::Rect,
    engine::{Descriptor, Engine, PickerSnapshot},
    interact::iced as iced_interact,
    render::{InputOwner, ReadValue, Skin, UiEvent, engine as engine_event},
    text::TextContext,
};

/// Wraps a control that already draws its own closed picker face, and gives
/// it the menu that face opens.
///
/// The face is part of the strip's picture, so the strip paints it; the menu
/// is a layer over everything, so it is raised here. Where the two meet is the
/// one rectangle the strip reports, rather than a second measurement.
pub(crate) fn scope_picker<'a>(
    path: &str,
    items: Vec<&'a str>,
    value: Option<&ReadValue<'_>>,
    skin: &'a Skin,
    owner: InputOwner,
    anchor: Element<'a, UiEvent>,
    face: impl Fn(Rect) -> Rect + 'a,
) -> Element<'a, UiEvent> {
    let selected = picker_selected_index(value, items.len());
    Element::new(PickerWidget {
        anchor,
        face: Box::new(face),
        owner,
        paint: Rc::new(PickerPaint::new(items, selected, skin)),
        path: path.to_owned(),
    })
}

pub(crate) fn sync_picker(path: &str, snapshot: PickerSnapshot) -> impl Operation + '_ {
    struct Sync<'a> {
        path: &'a str,
        snapshot: PickerSnapshot,
    }

    impl Operation for Sync<'_> {
        fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
            operate(self);
        }

        fn custom(&mut self, _id: Option<&widget::Id>, _bounds: Rectangle, state: &mut dyn Any) {
            if let Some(state) = state.downcast_mut::<PickerState>() {
                state.sync(self.path, self.snapshot);
            }
        }
    }

    Sync { path, snapshot }
}

struct PickerWidget<'a> {
    anchor: Element<'a, UiEvent>,
    face: Box<dyn Fn(Rect) -> Rect + 'a>,
    owner: InputOwner,
    paint: Rc<PickerPaint<'a>>,
    path: String,
}

impl PickerWidget<'_> {
    /// The part of the strip the picker answers for, in the window's own
    /// coordinates.
    fn face_bounds(&self, bounds: Rectangle) -> Rectangle {
        let face = (self.face)(bounds.into());
        Rectangle {
            height: face.h,
            width: face.w,
            x: face.x,
            y: face.y,
        }
    }

    /// One input through the leaf's own engine, which is what opens the menu
    /// and moves the highlight before the menu exists to answer for itself.
    fn open(
        &self,
        tree: &mut Tree,
        event: &Event,
        face: Rectangle,
        cursor: mouse::Cursor,
        shell: &mut Shell<'_, UiEvent>,
    ) {
        let Some(input) = iced_interact::input(event) else {
            return;
        };
        let state = tree.state.downcast_mut::<PickerState>();
        let targets = targets(
            &self.path,
            face,
            cursor,
            state.snapshot().open,
            self.paint.item_count(),
            self.paint.item_height(),
        );
        let emission = state
            .engine_mut()
            .and_then(|engine| engine.handle(input, &targets, Instant::now()));
        state.refresh(&self.path);
        let Some(action) = emission
            .and_then(|emission| engine_event(&emission.path, emission.child, emission.outcome))
        else {
            return;
        };
        let (message, redraw, status) = action.into_inner();
        shell.request_redraw_at(redraw);
        if let Some(message) = message {
            shell.publish(message);
        }
        if status == iced::event::Status::Captured {
            shell.capture_event();
        }
    }
}

impl IcedWidget<UiEvent, Theme, Renderer> for PickerWidget<'_> {
    fn tag(&self) -> widget::tree::Tag {
        widget::tree::Tag::of::<PickerState>()
    }

    fn state(&self) -> widget::tree::State {
        widget::tree::State::new(PickerState::new(
            &self.path,
            self.paint.item_count(),
            self.paint.selected(),
            self.owner,
        ))
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.anchor)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_ref(&self.anchor));
        tree.state.downcast_mut::<PickerState>().reconcile(
            &self.path,
            self.paint.item_count(),
            self.paint.selected(),
            self.owner,
        );
    }

    delegate::delegate! {
        to self.anchor.as_widget() {
            fn size(&self) -> Size<Length>;
            fn size_hint(&self) -> Size<Length>;
        }
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.anchor.as_widget().mouse_interaction(
            &tree.children[0],
            layout,
            cursor,
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
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        self.anchor.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout,
            cursor,
            viewport,
        );
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.anchor
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits)
    }

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
        let before = tree.state.downcast_ref::<PickerState>().snapshot();
        self.anchor.as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout,
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );
        if self.owner == InputOwner::Leaf {
            self.open(
                tree,
                event,
                self.face_bounds(layout.bounds()),
                cursor,
                shell,
            );
        }
        let after = tree.state.downcast_ref::<PickerState>().snapshot();
        if before != after {
            shell.request_redraw();
            shell.invalidate_layout();
        }
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        _renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        operation.custom(
            None,
            layout.bounds(),
            tree.state.downcast_mut::<PickerState>(),
        );
    }

    fn overlay<'a>(
        &'a mut self,
        tree: &'a mut Tree,
        layout: Layout<'a>,
        _renderer: &Renderer,
        _viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'a, UiEvent, Theme, Renderer>> {
        let anchor = self.face_bounds(layout.bounds()) + translation;
        let state = tree.state.downcast_mut::<PickerState>();
        state.snapshot().open.then(|| {
            overlay::Element::new(Box::new(PickerPortal {
                anchor,
                owner: self.owner,
                paint: &self.paint,
                path: &self.path,
                state,
            }))
        })
    }
}

#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub(super) struct PickerState {
    engine: Option<Engine>,
    path: String,
    #[field(get, vis = "pub(super)", copy)]
    snapshot: PickerSnapshot,
    #[field(get, vis = "pub(super)")]
    text: RefCell<Option<TextContext>>,
}

impl Default for PickerState {
    fn default() -> Self {
        Self {
            engine: None,
            path: String::new(),
            snapshot: PickerSnapshot {
                open: false,
                highlighted: None,
            },
            text: RefCell::default(),
        }
    }
}

impl PickerState {
    pub(super) fn new(
        path: &str,
        item_count: usize,
        selected: Option<usize>,
        owner: InputOwner,
    ) -> Self {
        let mut state = Self {
            engine: None,
            path: path.to_owned(),
            snapshot: PickerSnapshot {
                open: false,
                highlighted: selected.filter(|index| *index < item_count),
            },
            text: RefCell::default(),
        };
        state.reconcile(path, item_count, selected, owner);
        state
    }

    fn reconcile(
        &mut self,
        path: &str,
        item_count: usize,
        selected: Option<usize>,
        owner: InputOwner,
    ) {
        self.path = path.to_owned();
        match owner {
            InputOwner::Leaf => {
                self.engine
                    .get_or_insert_with(Engine::default)
                    .reconcile([Descriptor::picker(path.to_owned(), item_count, selected)]);
                self.refresh(path);
            }
            InputOwner::Engine => self.engine = None,
        }
    }

    pub(super) fn engine_mut(&mut self) -> Option<&mut Engine> {
        self.engine.as_mut()
    }

    pub(super) fn refresh(&mut self, path: &str) {
        if let Some(snapshot) = self
            .engine
            .as_ref()
            .and_then(|engine| engine.picker_snapshot(path))
        {
            self.snapshot = snapshot;
        }
    }

    fn sync(&mut self, path: &str, snapshot: PickerSnapshot) {
        if self.path == path {
            self.snapshot = snapshot;
        }
    }
}

#[cfg(test)]
mod tests {
    use iced::{
        Element, Event, Length, Pixels, Point, Rectangle, Renderer, Size, Vector,
        advanced::{
            Shell, clipboard,
            layout::{Layout, Limits},
            mouse, overlay,
            widget::Tree,
        },
        keyboard::{
            self, Location, Modifiers,
            key::{Code, Named, Physical},
        },
        widget::{Column, Space},
    };
    use iced_renderer::fallback::Renderer as FallbackRenderer;
    use iced_tiny_skia::Renderer as TinySkiaRenderer;
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        builtin,
        engine::PickerSnapshot,
        render::{ControlAction, WindowCommand, fonts::SANS},
        widgets::{Widget, window::WindowSurface},
    };

    fn key_event(key: Named, code: Code) -> Event {
        Event::Keyboard(keyboard::Event::KeyPressed {
            key: keyboard::Key::Named(key),
            modified_key: keyboard::Key::Named(key),
            physical_key: Physical::Code(code),
            location: Location::Standard,
            modifiers: Modifiers::empty(),
            text: None,
            repeat: false,
        })
    }

    /// The face the strip reports is where the picker listens, and the menu it
    /// opens is driven by the leaf's own engine — a press opens it, an arrow
    /// moves the highlight, and Enter picks. None of that reaches the strip's
    /// painter, which only draws the face where this says it is.
    #[kithara::test]
    fn a_leaf_picker_opens_navigates_and_selects_over_the_face_it_was_given() {
        let skin = builtin::skin();
        let face = Rect {
            h: skin.tree.scope_item_height,
            w: 72.0,
            x: 0.0,
            y: 0.0,
        };
        let mut element: Element<'_, UiEvent> = scope_picker(
            "library/context",
            vec!["ZVUK", "LOCAL"],
            None,
            skin,
            InputOwner::Leaf,
            Space::new()
                .width(Length::Fixed(face.w))
                .height(Length::Fixed(face.h))
                .into(),
            move |bounds: Rect| Rect {
                x: bounds.x,
                y: bounds.y,
                ..face
            },
        );
        let renderer = FallbackRenderer::Secondary(TinySkiaRenderer::new(SANS, Pixels(14.0)));
        let viewport = Size::new(160.0, face.h * 3.0);
        let mut tree = Tree::new(element.as_widget());
        let node = element.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &Limits::new(Size::ZERO, viewport),
        );
        let deliver = |element: &mut Element<'_, UiEvent>,
                       tree: &mut Tree,
                       event: Event,
                       cursor: mouse::Cursor| {
            let mut clipboard = clipboard::Null;
            let mut messages = Vec::new();
            let mut shell = Shell::new(&mut messages);
            element.as_widget_mut().update(
                tree,
                &event,
                Layout::new(&node),
                cursor,
                &renderer,
                &mut clipboard,
                &mut shell,
                &Rectangle::with_size(viewport),
            );
            drop(shell);
            messages
        };

        assert!(
            deliver(
                &mut element,
                &mut tree,
                Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                mouse::Cursor::Available(Point::new(40.0, face.h / 2.0)),
            )
            .is_empty(),
            "opening the menu says nothing to the document yet"
        );
        assert!(tree.state.downcast_ref::<PickerState>().snapshot().open);

        assert!(
            deliver(
                &mut element,
                &mut tree,
                key_event(Named::ArrowDown, Code::ArrowDown),
                mouse::Cursor::Unavailable,
            )
            .is_empty()
        );
        assert_eq!(
            tree.state
                .downcast_ref::<PickerState>()
                .snapshot()
                .highlighted,
            Some(1)
        );

        assert_eq!(
            deliver(
                &mut element,
                &mut tree,
                key_event(Named::Enter, Code::Enter),
                mouse::Cursor::Unavailable,
            ),
            [UiEvent::Control {
                path: "library/context".to_owned(),
                action: ControlAction::SelectIndex(1),
            }]
        );
        assert!(!tree.state.downcast_ref::<PickerState>().snapshot().open);
    }

    #[kithara::test]
    fn engine_owned_picker_state_has_no_local_engine() {
        let mut state = PickerState::new("library/context", 2, Some(0), InputOwner::Engine);

        assert!(state.engine_mut().is_none());
        assert_eq!(
            state.snapshot(),
            PickerSnapshot {
                open: false,
                highlighted: Some(0),
            }
        );
    }

    fn dispatch(
        element: &mut Element<'_, UiEvent>,
        tree: &mut Tree,
        node: &layout::Node,
        renderer: &Renderer,
        viewport: Size,
        pointer: Point,
    ) -> (Vec<UiEvent>, bool) {
        let event = Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left));
        let cursor = mouse::Cursor::Available(pointer);
        let bounds = Rectangle::with_size(viewport);
        let layout = Layout::new(node);
        let mut clipboard = clipboard::Null;
        let mut messages = Vec::new();
        let mut shell = Shell::new(&mut messages);
        let mut base_cursor = cursor;
        {
            let overlay =
                element
                    .as_widget_mut()
                    .overlay(tree, layout, renderer, &bounds, Vector::ZERO);
            if let Some(overlay) = overlay {
                let mut nested = overlay::Nested::new(overlay);
                let overlay_node = nested.layout(renderer, viewport);
                nested.update(
                    &event,
                    Layout::new(&overlay_node),
                    cursor,
                    renderer,
                    &mut clipboard,
                    &mut shell,
                );
                if !shell.is_event_captured()
                    && nested.mouse_interaction(Layout::new(&overlay_node), cursor, renderer)
                        != mouse::Interaction::None
                {
                    base_cursor = mouse::Cursor::Unavailable;
                }
            }
        }
        if !shell.is_event_captured() {
            element.as_widget_mut().update(
                tree,
                &event,
                layout,
                base_cursor,
                renderer,
                &mut clipboard,
                &mut shell,
                &bounds,
            );
        }
        let captured = shell.is_event_captured();
        drop(shell);
        (messages, captured)
    }

    #[kithara::test]
    fn an_open_popup_captures_before_an_overlapping_window_layer() {
        let skin = builtin::skin();
        let item_height = skin.tree.scope_item_height;
        let face = Rect {
            h: item_height,
            w: 72.0,
            x: 0.0,
            y: 0.0,
        };
        let picker = scope_picker(
            "library/context",
            vec!["ZVUK", "LOCAL"],
            None,
            skin,
            InputOwner::Leaf,
            Space::new()
                .width(Length::Fixed(face.w))
                .height(Length::Fixed(face.h))
                .into(),
            move |bounds: Rect| Rect {
                x: bounds.x,
                y: bounds.y,
                ..face
            },
        );
        let chrome = WindowSurface::drag().view();
        let mut element: Element<'_, UiEvent> = Column::with_children(vec![picker, chrome])
            .width(Length::Fill)
            .height(Length::Fill)
            .into();
        let renderer = FallbackRenderer::Secondary(TinySkiaRenderer::new(SANS, Pixels(14.0)));
        let viewport = Size::new(160.0, item_height * 3.0);
        let mut tree = Tree::new(element.as_widget());
        let node = element.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &Limits::new(Size::ZERO, viewport),
        );

        let (opened, captured) = dispatch(
            &mut element,
            &mut tree,
            &node,
            &renderer,
            viewport,
            Point::new(4.0, item_height / 2.0),
        );
        assert!(opened.is_empty());
        assert!(captured);

        let (selected, captured) = dispatch(
            &mut element,
            &mut tree,
            &node,
            &renderer,
            viewport,
            Point::new(4.0, item_height + item_height / 2.0),
        );
        assert_eq!(
            selected,
            [UiEvent::Control {
                path: "library/context".to_owned(),
                action: ControlAction::SelectIndex(0),
            }]
        );
        assert!(captured);
        assert!(!selected.contains(&UiEvent::Window(WindowCommand::Drag)));
    }
}
