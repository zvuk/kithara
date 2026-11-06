use iced::{Element, widget::canvas::Action};

use crate::{
    engine::EngineEvent,
    hosts::event::{carry_event, control_event},
    interact::{Outcome, recognizers::DragEvent},
    render::{Carry, ControlAction, Published, WindowCommand},
};

/// Shared view contract: a built control renders itself into the event tree.
pub(crate) trait Widget<'a> {
    fn view(self) -> Element<'a, Published>;
}

/// Carry a recognizer's verdict out to the toolkit. The recognizer decides
/// whether the gesture took the pointer; this only names the event it produced.
fn action<T>(outcome: Outcome<T>, event: impl FnOnce(T) -> Published) -> Option<Action<Published>> {
    let captured = outcome.is_captured();
    let Some(value) = outcome.value() else {
        return captured.then(Action::capture);
    };
    let published = Action::publish(event(value));
    Some(if captured {
        published.and_capture()
    } else {
        published
    })
}

pub(crate) fn publish(outcome: Outcome<Published>) -> Option<Action<Published>> {
    action(outcome, |event| event)
}

fn set_scalar(path: &str, value: f64) -> Published {
    control_event(path, ControlAction::SetScalar(value))
}

pub(crate) fn scalar(path: &str, outcome: Outcome<f64>) -> Option<Action<Published>> {
    action(outcome, |value| set_scalar(path, value))
}

pub(crate) fn step(path: &str, outcome: Outcome<ControlAction>) -> Option<Action<Published>> {
    action(outcome, |event| control_event(path, event))
}

pub(crate) fn activate(path: &str, outcome: Outcome<()>) -> Option<Action<Published>> {
    action(outcome, |()| control_event(path, ControlAction::Activate))
}

pub(crate) fn window(command: WindowCommand, outcome: Outcome<()>) -> Option<Action<Published>> {
    action(outcome, |()| Published::window(command))
}

pub(crate) fn index(path: &str, outcome: Outcome<usize>) -> Option<Action<Published>> {
    action(outcome, |index| {
        control_event(path, ControlAction::SelectIndex(index))
    })
}

pub(crate) fn engine(
    path: &str,
    child: Option<&str>,
    outcome: Outcome<EngineEvent>,
) -> Option<Action<Published>> {
    let captured = outcome.is_captured();
    match outcome.value() {
        Some(EngineEvent::Scalar(value)) => child.map_or_else(
            || scalar(path, typed_outcome(value, captured)),
            |child| Some(scalar_child(path, child, value)),
        ),
        Some(EngineEvent::Activate) => activate(path, typed_outcome((), captured)),
        Some(EngineEvent::Crossing(over)) => {
            carry(path, typed_outcome(Carry::Over(over), captured))
        }
        Some(EngineEvent::Index(selected)) => index(path, typed_outcome(selected, captured)),
        Some(EngineEvent::Drag { event, index }) => {
            drag(path, index, typed_outcome(event, captured))
        }
        Some(EngineEvent::Text(query)) => action(typed_outcome(query, captured), |query| {
            control_event(path, ControlAction::Text(query))
        }),
        None => captured.then(Action::capture),
    }
}

fn typed_outcome<T>(value: T, captured: bool) -> Outcome<T> {
    if captured {
        Outcome::set(value)
    } else {
        Outcome::observed(value)
    }
}

/// A value a control decides for itself, addressed under one of its own
/// endpoints rather than the one its gesture writes.
pub(crate) fn scalar_child(path: &str, child: &str, value: f64) -> Action<Published> {
    Action::publish(set_scalar(&format!("{path}/{child}"), value)).and_capture()
}

pub(crate) fn drag(
    path: &str,
    index: usize,
    outcome: Outcome<DragEvent>,
) -> Option<Action<Published>> {
    carry(
        path,
        outcome.map(|event| match event {
            DragEvent::Started => Carry::Start(index),
            DragEvent::Dropped => Carry::Drop,
        }),
    )
}

fn carry(path: &str, outcome: Outcome<Carry>) -> Option<Action<Published>> {
    action(outcome, |step| carry_event(path, step))
}

#[cfg(test)]
mod tests {
    use iced::{event, window::RedrawRequest};
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn an_engine_child_emission_binds_under_the_control_path() {
        let action = engine(
            "deck-a/wave",
            Some("loop_start"),
            Outcome::set(EngineEvent::Scalar(0.14)),
        )
        .expect("a child scalar emission must publish");

        assert_eq!(
            action.into_inner().0,
            Some(Published::Gesture {
                path: "deck-a/wave/loop_start".to_owned(),
                action: ControlAction::SetScalar(0.14),
            })
        );
    }

    #[kithara::test]
    fn an_engine_index_emission_binds_to_select_index() {
        let action = engine("cells/beat", None, Outcome::set(EngineEvent::Index(3)))
            .expect("an index emission must publish");

        assert_eq!(
            action.into_inner().0,
            Some(Published::Gesture {
                path: "cells/beat".to_owned(),
                action: ControlAction::SelectIndex(3),
            })
        );
    }

    #[kithara::test]
    fn an_engine_item_drag_uses_the_existing_index_binder() {
        let action = engine(
            "library/tracks",
            None,
            Outcome::observed(EngineEvent::Drag {
                event: DragEvent::Started,
                index: 3,
            }),
        )
        .expect("an item drag must publish through the existing binder");

        assert_eq!(
            action.into_inner().0,
            Some(Published::Carry {
                path: "library/tracks".to_owned(),
                step: Carry::Start(3),
            })
        );
    }

    #[kithara::test]
    fn engine_crossings_bind_to_uncaptured_drag_over_events() {
        for over in [true, false] {
            let action = engine(
                "deck-a/drop",
                None,
                Outcome::observed(EngineEvent::Crossing(over)),
            )
            .expect("a crossing must publish");

            assert_eq!(
                action.into_inner(),
                (
                    Some(Published::Carry {
                        path: "deck-a/drop".to_owned(),
                        step: Carry::Over(over),
                    }),
                    RedrawRequest::Wait,
                    event::Status::Ignored,
                )
            );
        }
    }
}
