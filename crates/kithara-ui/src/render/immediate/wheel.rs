use iced::{
    Element, Event, Length, Rectangle, Renderer, Theme,
    mouse::{self, Cursor},
    widget::canvas::{self, Action, Canvas, Geometry},
};
use kithara_platform::time::Instant;

use crate::{
    interact::{CursorShape, Hover, iced as iced_interact, recognizers::Stepper},
    render::{UiEvent, Widget, step},
};

#[derive(bon::Builder)]
pub(crate) struct WheelSurface<'path> {
    path: &'path str,
}

impl<'a> Widget<'a> for WheelSurface<'_> {
    fn view(self) -> Element<'a, UiEvent> {
        Canvas::new(WheelCanvas {
            path: self.path.to_owned(),
        })
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
    }
}

struct WheelCanvas {
    path: String,
}

impl canvas::Program<UiEvent> for WheelCanvas {
    type State = Stepper;

    fn draw(
        &self,
        _state: &Stepper,
        _renderer: &Renderer,
        _theme: &Theme,
        _bounds: Rectangle,
        _cursor: Cursor,
    ) -> Vec<Geometry> {
        Vec::new()
    }

    fn mouse_interaction(
        &self,
        state: &Stepper,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> mouse::Interaction {
        Hover::new(CursorShape::ResizeV)
            .cursor(state.dragging(), &iced_interact::hit(bounds, cursor))
            .into()
    }

    fn update(
        &self,
        state: &mut Stepper,
        event: &Event,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> Option<Action<UiEvent>> {
        let input = iced_interact::input(event)?;
        let hit = iced_interact::hit(bounds, cursor);
        step(&self.path, state.on_input(input, &hit, Instant::now()))
    }
}

#[cfg(test)]
mod tests {
    use iced::{Point, Size, mouse::ScrollDelta, widget::canvas::Program};
    use kithara_test_utils::kithara;

    use super::*;
    use crate::render::ControlAction;

    #[kithara::test]
    fn the_surface_publishes_its_step_under_its_own_path() {
        let surface = WheelCanvas {
            path: "deck-a/tempo".to_owned(),
        };
        let bounds = Rectangle::new(Point::ORIGIN, Size::new(80.0, 38.0));
        let cursor = Cursor::Available(Point::new(40.0, 19.0));
        let detent = Event::Mouse(mouse::Event::WheelScrolled {
            delta: ScrollDelta::Lines { x: 0.0, y: -1.0 },
        });

        let action = surface
            .update(&mut Stepper::default(), &detent, bounds, cursor)
            .expect("a detent over the surface must publish");

        assert_eq!(
            action.into_inner().0,
            Some(UiEvent::Control {
                path: "deck-a/tempo".to_owned(),
                action: ControlAction::StepScalar(1.0),
            })
        );
    }
}
