use iced::{
    Element, Event, Length, Rectangle, Renderer, Theme,
    mouse::{self, Cursor},
    widget::canvas::{self, Action, Canvas, Frame, Geometry},
    window,
};
use kithara_platform::time::Instant;

use crate::{
    draw::{DrawListBuilder, Rect},
    hosts::surface::{Entry, SteppingSurface},
    iced::tree::{Widget, step},
    interact::iced as iced_interact,
    render::{Published, Skin},
};

#[derive(bon::Builder)]
pub(crate) struct WheelSurface<'path, 'skin> {
    path: &'path str,
    entry: Option<(String, &'skin Skin)>,
}

impl<'a> Widget<'a> for WheelSurface<'_, 'a> {
    fn view(self) -> Element<'a, Published> {
        Canvas::new(WheelCanvas {
            path: self.path.to_owned(),
            entry: self.entry,
        })
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
    }
}

struct WheelCanvas<'skin> {
    path: String,
    entry: Option<(String, &'skin Skin)>,
}

impl canvas::Program<Published> for WheelCanvas<'_> {
    type State = SteppingSurface;

    fn draw(
        &self,
        state: &SteppingSurface,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: Cursor,
    ) -> Vec<Geometry> {
        let Some((_, skin)) = self.entry.as_ref().filter(|_| state.editing()) else {
            return Vec::new();
        };
        let mut frame = Frame::new(renderer, bounds.size());
        let mut list = DrawListBuilder::default();
        state.paint(
            &mut list,
            Rect {
                x: 0.0,
                y: 0.0,
                w: bounds.width,
                h: bounds.height,
            },
            skin,
        );
        crate::backends::replay_ordered(&list.finish(), &mut frame, skin.text_resources.as_ref());
        vec![frame.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        state: &SteppingSurface,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> mouse::Interaction {
        state.cursor(&iced_interact::hit(bounds, cursor)).into()
    }

    fn update(
        &self,
        state: &mut SteppingSurface,
        event: &Event,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> Option<Action<Published>> {
        if matches!(event, Event::Window(window::Event::Unfocused)) {
            return state.close().then(Action::request_redraw);
        }
        let input = iced_interact::input(event)?;
        let hit = iced_interact::hit(bounds, cursor);
        let editing = state.editing();
        let entry = self
            .entry
            .as_ref()
            .map(|(value, skin)| Entry { value, skin });
        let outcome = state.on_input(input, &hit, Instant::now(), entry);
        if (editing || state.editing()) && outcome.clone().value().is_none() {
            return Some(if outcome.is_captured() {
                Action::request_redraw().and_capture()
            } else {
                Action::request_redraw()
            });
        }
        step(&self.path, outcome)
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
            entry: None,
        };
        let bounds = Rectangle::new(Point::ORIGIN, Size::new(80.0, 38.0));
        let cursor = Cursor::Available(Point::new(40.0, 19.0));
        let detent = Event::Mouse(mouse::Event::WheelScrolled {
            delta: ScrollDelta::Lines { x: 0.0, y: -1.0 },
        });

        let action = surface
            .update(&mut SteppingSurface::default(), &detent, bounds, cursor)
            .expect("a detent over the surface must publish");

        assert_eq!(
            action.into_inner().0,
            Some(Published::Gesture {
                path: "deck-a/tempo".to_owned(),
                action: ControlAction::StepScalar(1.0),
            })
        );
    }
}
