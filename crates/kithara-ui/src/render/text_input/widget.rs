use std::any::Any;

use iced::{
    Element, Event, Length, Rectangle, Renderer, Size, Theme,
    advanced::{
        Clipboard, Shell, Widget as IcedWidget,
        layout::{self, Layout},
        mouse, renderer,
        widget::{self, Operation, Tree},
    },
    widget::canvas::{Canvas, Program},
    window,
};

use super::{
    paint::TextInputPaint,
    program::{InputProgram, PaintProgram},
};
use crate::{
    engine::{Descriptor, Engine, Target, TextInputSnapshot},
    interact::{Hit, InputMethodRequest, TextInputLayout, iced as iced_interact},
    render::{InputOwner, Skin, UiEvent},
};

pub(crate) fn search_input<'a>(
    path: &str,
    query: &str,
    skin: &'a Skin,
    owner: InputOwner,
) -> Element<'a, UiEvent> {
    let paint = TextInputPaint::new(query, skin);
    let input_layout = paint.layout();
    match owner {
        InputOwner::Leaf => TextInputWidget::new(
            Canvas::new(InputProgram::new(path, paint))
                .width(Length::Fill)
                .height(Length::Fill),
            path,
            query,
            input_layout,
            owner,
        )
        .view(),
        InputOwner::Engine => TextInputWidget::new(
            Canvas::new(PaintProgram::new(paint))
                .width(Length::Fill)
                .height(Length::Fill),
            path,
            query,
            input_layout,
            owner,
        )
        .view(),
    }
}

pub(crate) fn sync_text_input(path: &str, snapshot: TextInputSnapshot) -> impl Operation + '_ {
    struct Sync<'a> {
        path: &'a str,
        snapshot: TextInputSnapshot,
    }

    impl Operation for Sync<'_> {
        fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
            operate(self);
        }

        fn custom(&mut self, _id: Option<&widget::Id>, _bounds: Rectangle, state: &mut dyn Any) {
            if let Some(state) = state.downcast_mut::<TextInputState>() {
                state.sync(self.path, self.snapshot.clone());
            }
        }
    }

    Sync { path, snapshot }
}

struct TextInputWidget<P>
where
    P: Program<UiEvent, Theme, Renderer, State = TextInputState>,
{
    canvas: Canvas<P, UiEvent>,
    input_layout: TextInputLayout,
    owner: InputOwner,
    path: String,
    query: String,
}

impl<P> TextInputWidget<P>
where
    P: Program<UiEvent, Theme, Renderer, State = TextInputState>,
{
    fn new(
        canvas: Canvas<P, UiEvent>,
        path: &str,
        query: &str,
        input_layout: TextInputLayout,
        owner: InputOwner,
    ) -> Self {
        Self {
            canvas,
            input_layout,
            owner,
            path: path.to_owned(),
            query: query.to_owned(),
        }
    }

    fn view<'a>(self) -> Element<'a, UiEvent>
    where
        P: 'a,
    {
        Element::new(self)
    }
}

impl<P> IcedWidget<UiEvent, Theme, Renderer> for TextInputWidget<P>
where
    P: Program<UiEvent, Theme, Renderer, State = TextInputState>,
{
    fn tag(&self) -> widget::tree::Tag {
        widget::tree::Tag::of::<TextInputState>()
    }

    fn state(&self) -> widget::tree::State {
        widget::tree::State::new(TextInputState::new(
            &self.path,
            &self.query,
            self.input_layout.clone(),
            self.owner,
        ))
    }

    fn diff(&self, tree: &mut Tree) {
        tree.state.downcast_mut::<TextInputState>().reconcile(
            &self.path,
            &self.query,
            self.input_layout.clone(),
            self.owner,
        );
    }

    delegate::delegate! {
        to self.canvas {
            fn size(&self) -> Size<Length>;
            fn size_hint(&self) -> Size<Length>;
            fn layout(
                &mut self,
                tree: &mut Tree,
                renderer: &Renderer,
                limits: &layout::Limits,
            ) -> layout::Node;
            fn mouse_interaction(
                &self,
                tree: &Tree,
                layout: Layout<'_>,
                cursor: mouse::Cursor,
                viewport: &Rectangle,
                renderer: &Renderer,
            ) -> mouse::Interaction;
            fn draw(
                &self,
                tree: &Tree,
                renderer: &mut Renderer,
                theme: &Theme,
                style: &renderer::Style,
                layout: Layout<'_>,
                cursor: mouse::Cursor,
                viewport: &Rectangle,
            );
        }
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
        self.canvas.update(
            tree, event, layout, cursor, renderer, clipboard, shell, viewport,
        );
        if matches!(self.owner, InputOwner::Leaf)
            && matches!(event, Event::Window(window::Event::RedrawRequested(_)))
        {
            let state = tree.state.downcast_ref::<TextInputState>();
            let request = iced_interact::input_method(state.input_method(layout.bounds()));
            shell.request_input_method(&request);
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
            tree.state.downcast_mut::<TextInputState>(),
        );
    }
}

#[derive(Default, fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub(super) struct TextInputState {
    engine: Option<Engine>,
    input_layout: TextInputLayout,
    path: String,
    #[field(get, vis = "pub(super)")]
    snapshot: TextInputSnapshot,
}

impl TextInputState {
    fn new(path: &str, query: &str, input_layout: TextInputLayout, owner: InputOwner) -> Self {
        let mut state = Self::default();
        state.reconcile(path, query, input_layout, owner);
        state
    }

    fn reconcile(
        &mut self,
        path: &str,
        query: &str,
        input_layout: TextInputLayout,
        owner: InputOwner,
    ) {
        let path_changed = self.path != path;
        let was_leaf = self.engine.is_some();
        self.path = path.to_owned();
        self.input_layout = input_layout;
        match owner {
            InputOwner::Leaf => {
                self.engine.get_or_insert_with(Engine::default).reconcile([
                    Descriptor::text_input(
                        self.path.clone(),
                        query.to_owned(),
                        self.input_layout.clone(),
                    ),
                ]);
                self.refresh();
            }
            InputOwner::Engine => {
                self.engine = None;
                if path_changed || was_leaf {
                    self.snapshot = TextInputSnapshot::default();
                }
            }
        }
    }

    delegate::delegate! {
        to self.engine {
            #[call(as_ref)]
            pub(super) const fn engine(&self) -> Option<&Engine>;
            #[call(as_mut)]
            pub(super) fn engine_mut(&mut self) -> Option<&mut Engine>;
        }
    }

    pub(super) fn refresh(&mut self) {
        if let Some(snapshot) = self
            .engine
            .as_ref()
            .and_then(|engine| engine.text_input_snapshot(&self.path))
        {
            self.snapshot = snapshot;
        }
    }

    fn input_method(&self, bounds: Rectangle) -> Option<InputMethodRequest<'_>> {
        let engine = self.engine.as_ref()?;
        let target = Target::new(&self.path, Hit::new(None, bounds.into()));
        engine.input_method(&[target])
    }

    fn sync(&mut self, path: &str, snapshot: TextInputSnapshot) {
        if self.path == path {
            self.snapshot = snapshot;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use iced::{
        Pixels, Point,
        advanced::{
            InputMethod as IcedInputMethod, clipboard, graphics::text::font_system, input_method,
            layout::Limits, widget::Tree,
        },
        keyboard::{
            self, Location, Modifiers,
            key::{Code, Physical},
        },
        mouse::{self, Button, Cursor},
        window,
    };
    use iced_renderer::fallback::Renderer as FallbackRenderer;
    use iced_tiny_skia::Renderer as TinySkiaRenderer;
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        builtin,
        render::{
            UiEvent,
            fonts::{FONT_BYTES, SANS},
        },
    };

    fn renderer() -> Renderer {
        let mut fonts = font_system()
            .write()
            .unwrap_or_else(|error| panic!("iced font system lock must be available: {error}"));
        for bytes in FONT_BYTES {
            fonts.load_font(Cow::Borrowed(bytes));
        }
        drop(fonts);
        FallbackRenderer::Secondary(TinySkiaRenderer::new(SANS, Pixels(14.0)))
    }

    fn character_event(character: &str, code: Code) -> Event {
        Event::Keyboard(keyboard::Event::KeyPressed {
            key: keyboard::Key::Character(character.into()),
            modified_key: keyboard::Key::Character(character.into()),
            physical_key: Physical::Code(code),
            location: Location::Standard,
            modifiers: Modifiers::empty(),
            text: Some(character.into()),
            repeat: false,
        })
    }

    fn redraw_event() -> Event {
        Event::Window(window::Event::RedrawRequested(iced::time::Instant::now()))
    }

    #[kithara::test]
    fn leaf_search_types_and_answers_composition_through_its_local_engine() {
        let renderer = renderer();
        let viewport = Size::new(180.0, builtin::skin().tree.search_height);
        let viewport_bounds = Rectangle::with_size(viewport);
        let mut element = search_input(
            "tree/browser/search",
            "ab",
            builtin::skin(),
            InputOwner::Leaf,
        );
        let mut tree = Tree::new(element.as_widget());
        let node = element.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &Limits::new(Size::ZERO, viewport),
        );
        let pointer = Cursor::Available(Point::new(viewport.width - 1.0, viewport.height / 2.0));
        let mut clipboard = clipboard::Null;
        let mut messages = Vec::new();

        for event in [
            Event::Mouse(mouse::Event::ButtonPressed(Button::Left)),
            Event::Mouse(mouse::Event::ButtonReleased(Button::Left)),
        ] {
            let mut shell = Shell::new(&mut messages);
            element.as_widget_mut().update(
                &mut tree,
                &event,
                Layout::new(&node),
                pointer,
                &renderer,
                &mut clipboard,
                &mut shell,
                &viewport_bounds,
            );
            assert!(shell.is_event_captured());
        }

        let mut shell = Shell::new(&mut messages);
        element.as_widget_mut().update(
            &mut tree,
            &character_event("x", Code::KeyX),
            Layout::new(&node),
            Cursor::Unavailable,
            &renderer,
            &mut clipboard,
            &mut shell,
            &viewport_bounds,
        );
        assert!(shell.is_event_captured());
        drop(shell);
        assert_eq!(messages, [UiEvent::LibraryQuery("abx".to_owned())]);

        let mut shell = Shell::new(&mut messages);
        element.as_widget_mut().update(
            &mut tree,
            &Event::InputMethod(input_method::Event::Preedit("日本".to_owned(), Some(3..6))),
            Layout::new(&node),
            Cursor::Unavailable,
            &renderer,
            &mut clipboard,
            &mut shell,
            &viewport_bounds,
        );
        assert!(shell.is_event_captured());
        drop(shell);
        assert_eq!(messages.len(), 1, "preedit must not publish");

        let mut shell = Shell::new(&mut messages);
        element.as_widget_mut().update(
            &mut tree,
            &redraw_event(),
            Layout::new(&node),
            Cursor::Unavailable,
            &renderer,
            &mut clipboard,
            &mut shell,
            &viewport_bounds,
        );
        match shell.input_method() {
            IcedInputMethod::Enabled {
                preedit: Some(preedit),
                ..
            } => assert_eq!(preedit.content, "日本"),
            request => panic!("leaf preedit must reach iced: {request:?}"),
        }
        drop(shell);

        let mut shell = Shell::new(&mut messages);
        element.as_widget_mut().update(
            &mut tree,
            &Event::InputMethod(input_method::Event::Commit("日".to_owned())),
            Layout::new(&node),
            Cursor::Unavailable,
            &renderer,
            &mut clipboard,
            &mut shell,
            &viewport_bounds,
        );
        assert!(shell.is_event_captured());
        drop(shell);
        assert_eq!(
            messages,
            [
                UiEvent::LibraryQuery("abx".to_owned()),
                UiEvent::LibraryQuery("abx日".to_owned()),
            ]
        );
    }
}
