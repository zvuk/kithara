use iced::{
    Event, Rectangle, Renderer, Theme,
    mouse::{Cursor, Interaction},
    widget::canvas::{self, Action, Geometry},
};
use kithara_platform::time::Instant;

use super::{paint::TextInputPaint, widget::TextInputState};
use crate::{
    engine::Target,
    interact::iced as iced_interact,
    render::{UiEvent, engine as engine_event},
};

pub(super) struct InputProgram<'a> {
    paint: TextInputPaint<'a>,
    path: String,
}

impl<'a> InputProgram<'a> {
    pub(super) fn new(path: &str, paint: TextInputPaint<'a>) -> Self {
        Self {
            paint,
            path: path.to_owned(),
        }
    }
}

impl canvas::Program<UiEvent> for InputProgram<'_> {
    type State = TextInputState;

    fn update(
        &self,
        state: &mut TextInputState,
        event: &Event,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> Option<Action<UiEvent>> {
        let input = iced_interact::input(event)?;
        let before = state.snapshot().clone();
        let target = Target::new(&self.path, iced_interact::hit(bounds, cursor));
        let emission = state.engine_mut()?.handle(input, &[target], Instant::now());
        state.refresh();
        let changed = before != *state.snapshot();
        let Some(emission) = emission else {
            return changed.then(Action::request_redraw);
        };
        if changed && emission.outcome.clone().value().is_none() {
            return Some(if emission.outcome.is_captured() {
                Action::request_redraw().and_capture()
            } else {
                Action::request_redraw()
            });
        }
        engine_event(&emission.path, emission.child, emission.outcome)
    }

    fn draw(
        &self,
        state: &TextInputState,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: Cursor,
    ) -> Vec<Geometry> {
        self.paint.geometry(state.snapshot(), renderer, bounds)
    }

    fn mouse_interaction(
        &self,
        state: &TextInputState,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> Interaction {
        let target = Target::new(&self.path, iced_interact::hit(bounds, cursor));
        state
            .engine()
            .map_or(Interaction::None, |engine| engine.cursor(&[target]).into())
    }
}

pub(super) struct PaintProgram<'a> {
    paint: TextInputPaint<'a>,
}

impl<'a> PaintProgram<'a> {
    pub(super) const fn new(paint: TextInputPaint<'a>) -> Self {
        Self { paint }
    }
}

impl canvas::Program<UiEvent> for PaintProgram<'_> {
    type State = TextInputState;

    fn draw(
        &self,
        state: &TextInputState,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: Cursor,
    ) -> Vec<Geometry> {
        self.paint.geometry(state.snapshot(), renderer, bounds)
    }
}
