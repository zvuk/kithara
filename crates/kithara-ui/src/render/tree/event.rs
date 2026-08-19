use iced::{Element, widget::canvas::Action};

use crate::{
    engine::EngineEvent,
    interact::{
        Outcome,
        recognizers::{DragEvent, StepEvent},
    },
    render::{ControlAction, DragPhase, UiEvent, WindowCommand, control_event},
};

/// Shared view contract: a built control renders itself into the event tree.
pub(crate) trait Widget<'a> {
    fn view(self) -> Element<'a, UiEvent>;
}

/// Carry a recognizer's verdict out to the toolkit. The recognizer decides
/// whether the gesture took the pointer; this only names the event it produced.
fn action<T>(outcome: Outcome<T>, event: impl FnOnce(T) -> UiEvent) -> Option<Action<UiEvent>> {
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

pub(crate) fn publish(outcome: Outcome<UiEvent>) -> Option<Action<UiEvent>> {
    action(outcome, |event| event)
}

fn set_scalar(path: &str, value: f64) -> UiEvent {
    control_event(path, ControlAction::SetScalar(value))
}

pub(crate) fn scalar(path: &str, outcome: Outcome<f64>) -> Option<Action<UiEvent>> {
    action(outcome, |value| set_scalar(path, value))
}

pub(crate) fn step(path: &str, outcome: Outcome<StepEvent>) -> Option<Action<UiEvent>> {
    action(outcome, |event| {
        control_event(
            path,
            match event {
                StepEvent::By(steps) => ControlAction::StepScalar(steps),
                StepEvent::Activate => ControlAction::Activate,
            },
        )
    })
}

pub(crate) fn activate(path: &str, outcome: Outcome<()>) -> Option<Action<UiEvent>> {
    action(outcome, |()| control_event(path, ControlAction::Activate))
}

pub(crate) fn toggle_module(module: &str, outcome: Outcome<()>) -> Option<Action<UiEvent>> {
    action(outcome, |()| UiEvent::ToggleModule(module.to_owned()))
}

pub(crate) fn window(command: WindowCommand, outcome: Outcome<()>) -> Option<Action<UiEvent>> {
    action(outcome, |()| UiEvent::Window(command))
}

/// What a control says to the document rather than to its own endpoint. The
/// event is built only once the gesture produced one, so a command that carries
/// a word costs nothing on the inputs that are not it.
pub(crate) fn command(event: fn() -> UiEvent, outcome: Outcome<()>) -> Option<Action<UiEvent>> {
    action(outcome, |()| event())
}

pub(crate) fn index(path: &str, outcome: Outcome<usize>) -> Option<Action<UiEvent>> {
    action(outcome, |index| {
        control_event(path, ControlAction::SelectIndex(index))
    })
}

pub(crate) fn engine(
    path: &str,
    child: Option<&str>,
    outcome: Outcome<EngineEvent>,
) -> Option<Action<UiEvent>> {
    let captured = outcome.is_captured();
    match outcome.value() {
        Some(EngineEvent::Scalar(value)) => child.map_or_else(
            || scalar(path, typed_outcome(value, captured)),
            |child| Some(scalar_child(path, child, value)),
        ),
        Some(EngineEvent::Activate) => activate(path, typed_outcome((), captured)),
        Some(EngineEvent::Crossing(over)) => {
            drag_phase(path, typed_outcome(DragPhase::Over(over), captured))
        }
        Some(EngineEvent::Index(selected)) => index(path, typed_outcome(selected, captured)),
        Some(EngineEvent::Drag { event, index }) => {
            drag(path, index, typed_outcome(event, captured))
        }
        Some(EngineEvent::Text(query)) => {
            action(typed_outcome(query, captured), UiEvent::LibraryQuery)
        }
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
pub(crate) fn scalar_child(path: &str, child: &str, value: f64) -> Action<UiEvent> {
    Action::publish(set_scalar(&format!("{path}/{child}"), value)).and_capture()
}

pub(crate) fn drag(
    path: &str,
    index: usize,
    outcome: Outcome<DragEvent>,
) -> Option<Action<UiEvent>> {
    drag_phase(
        path,
        outcome.map(|event| match event {
            DragEvent::Started => DragPhase::Start(index),
            DragEvent::Dropped => DragPhase::Drop,
        }),
    )
}

fn drag_phase(path: &str, outcome: Outcome<DragPhase>) -> Option<Action<UiEvent>> {
    action(outcome, |phase| {
        control_event(path, ControlAction::Drag(phase))
    })
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
        .unwrap_or_else(|| panic!("a child scalar emission must publish"));

        assert_eq!(
            action.into_inner().0,
            Some(UiEvent::Control {
                path: "deck-a/wave/loop_start".to_owned(),
                action: ControlAction::SetScalar(0.14),
            })
        );
    }

    #[kithara::test]
    fn an_engine_index_emission_binds_to_select_index() {
        let action = engine("cells/beat", None, Outcome::set(EngineEvent::Index(3)))
            .unwrap_or_else(|| panic!("an index emission must publish"));

        assert_eq!(
            action.into_inner().0,
            Some(UiEvent::Control {
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
        .unwrap_or_else(|| panic!("an item drag must publish through the existing binder"));

        assert_eq!(
            action.into_inner().0,
            Some(UiEvent::Control {
                path: "library/tracks".to_owned(),
                action: ControlAction::Drag(DragPhase::Start(3)),
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
            .unwrap_or_else(|| panic!("a crossing must publish"));

            assert_eq!(
                action.into_inner(),
                (
                    Some(UiEvent::Control {
                        path: "deck-a/drop".to_owned(),
                        action: ControlAction::Drag(DragPhase::Over(over)),
                    }),
                    RedrawRequest::Wait,
                    event::Status::Ignored,
                )
            );
        }
    }

    #[kithara::test]
    fn module_header_activation_binds_directly_to_toggle_module() {
        let action = toggle_module("app-deck", Outcome::set(()))
            .unwrap_or_else(|| panic!("a module-header activation must publish"));

        assert_eq!(
            action.into_inner().0,
            Some(UiEvent::ToggleModule("app-deck".to_owned()))
        );
    }
}
