use iced::{
    Element, Event, Length, Rectangle, Renderer, Size, Theme,
    advanced::{
        Clipboard, Shell, Widget as IcedWidget,
        layout::{self, Layout},
        mouse, renderer,
        widget::{Operation, Tree},
    },
    widget::canvas::{Canvas, Program},
};

use crate::render::UiEvent;

pub(in crate::render) trait RetainedCanvasState: Default {
    type Config;

    fn reconcile_canvas(&mut self, path: &str, config: &Self::Config);
    fn set_canvas_viewport(&mut self, size: Size, config: &Self::Config);
}

pub(in crate::render) struct RetainedCanvas<P>
where
    P: Program<UiEvent, Theme, Renderer>,
    P::State: RetainedCanvasState,
{
    canvas: Canvas<P, UiEvent>,
    config: <P::State as RetainedCanvasState>::Config,
    path: String,
}

impl<P> RetainedCanvas<P>
where
    P: Program<UiEvent, Theme, Renderer>,
    P::State: RetainedCanvasState,
{
    pub(in crate::render) fn new(
        program: P,
        path: &str,
        config: <P::State as RetainedCanvasState>::Config,
    ) -> Self {
        Self {
            canvas: Canvas::new(program)
                .width(Length::Fill)
                .height(Length::Fill),
            config,
            path: path.to_owned(),
        }
    }

    pub(in crate::render) fn view<'a>(self) -> Element<'a, UiEvent>
    where
        P: 'a,
    {
        Element::new(self)
    }
}

impl<P> IcedWidget<UiEvent, Theme, Renderer> for RetainedCanvas<P>
where
    P: Program<UiEvent, Theme, Renderer>,
    P::State: RetainedCanvasState,
{
    delegate::delegate! {
        to self.canvas {
            fn tag(&self) -> iced::advanced::widget::tree::Tag;
            fn size(&self) -> Size<Length>;
            fn size_hint(&self) -> Size<Length>;
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
            );
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

    fn state(&self) -> iced::advanced::widget::tree::State {
        let mut state = P::State::default();
        state.reconcile_canvas(&self.path, &self.config);
        iced::advanced::widget::tree::State::new(state)
    }

    fn diff(&self, tree: &mut Tree) {
        self.canvas.diff(tree);
        tree.state
            .downcast_mut::<P::State>()
            .reconcile_canvas(&self.path, &self.config);
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let node = self.canvas.layout(tree, renderer, limits);
        tree.state
            .downcast_mut::<P::State>()
            .set_canvas_viewport(node.size(), &self.config);
        node
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        _renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        operation.custom(None, layout.bounds(), tree.state.downcast_mut::<P::State>());
    }
}
