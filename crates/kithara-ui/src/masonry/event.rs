use crate::{
    engine::EngineEvent,
    hosts::event::{carry_event, control_event},
    interact::recognizers::DragEvent,
    render::{Carry, ControlAction, Published},
};

pub(crate) fn engine_value(path: &str, child: Option<&str>, event: EngineEvent) -> Published {
    match event {
        EngineEvent::Scalar(value) => {
            let path = child.map_or_else(|| path.to_owned(), |child| format!("{path}/{child}"));
            control_event(&path, ControlAction::SetScalar(value))
        }
        EngineEvent::Activate => control_event(path, ControlAction::Activate),
        EngineEvent::SecondaryActivate => control_event(path, ControlAction::SecondaryActivate),
        EngineEvent::DoubleActivate => control_event(path, ControlAction::DoubleActivate),
        EngineEvent::Crossing(over) => carry_event(path, Carry::Over(over)),
        EngineEvent::Index(selected) => control_event(path, ControlAction::SelectIndex(selected)),
        EngineEvent::Drag { event, index } => carry_event(
            path,
            match event {
                DragEvent::Started => Carry::Start(index),
                DragEvent::Dropped => Carry::Drop,
            },
        ),
        EngineEvent::Text(query) => control_event(path, ControlAction::Text(query)),
    }
}
