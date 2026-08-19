use crate::interact::recognizers::Edge;
#[cfg(feature = "masonry")]
use crate::{engine::EngineEvent, interact::recognizers::DragEvent};

/// The one place a control event is built. Every publisher and every widget
/// goes through here, so a binding rule has a single site to attach to instead
/// of fifteen literals to keep in step.
pub(crate) fn control_event(path: &str, action: ControlAction) -> UiEvent {
    UiEvent::Control {
        action,
        path: path.to_owned(),
    }
}

/// Where one end of a two-handled interval publishes.
///
/// Both ends are host-owned scalars under the control's own path, so the
/// control needs no second document node and the host needs no rule for
/// turning an index back into a name.
pub(crate) fn span_event(path: &str, edge: Edge, value: f32) -> UiEvent {
    let child = match edge {
        Edge::Min => "min",
        Edge::Max => "max",
    };
    control_event(
        &format!("{path}/{child}"),
        ControlAction::SetScalar(f64::from(value)),
    )
}

#[cfg(feature = "masonry")]
pub(crate) fn engine_value(path: &str, child: Option<&str>, event: EngineEvent) -> UiEvent {
    match event {
        EngineEvent::Scalar(value) => {
            let path = child.map_or_else(|| path.to_owned(), |child| format!("{path}/{child}"));
            control_event(&path, ControlAction::SetScalar(value))
        }
        EngineEvent::Activate => control_event(path, ControlAction::Activate),
        EngineEvent::Crossing(over) => {
            control_event(path, ControlAction::Drag(DragPhase::Over(over)))
        }
        EngineEvent::Index(selected) => control_event(path, ControlAction::SelectIndex(selected)),
        EngineEvent::Drag { event, index } => control_event(
            path,
            ControlAction::Drag(match event {
                DragEvent::Started => DragPhase::Start(index),
                DragEvent::Dropped => DragPhase::Drop,
            }),
        ),
        EngineEvent::Text(query) => UiEvent::LibraryQuery(query),
    }
}

/// Action emitted by an interactive control.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum ControlAction {
    Activate,
    SecondaryActivate,
    SetScalar(f64),
    StepScalar(f32),
    SelectIndex(usize),
    Drag(DragPhase),
}

/// Phase of a pointer drag that carries an item from the control it started on
/// to the one it is released over.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum DragPhase {
    /// The item at this index is now being dragged out of the control.
    Start(usize),
    /// The pointer crossed into (`true`) or out of (`false`) the control.
    Over(bool),
    /// The pointer was released and the drag ended.
    Drop,
}

/// Command emitted by portable window-chrome controls and executed by the host.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum WindowCommand {
    Drag,
    Resize(WindowEdge),
    Minimize,
    ToggleMaximize,
    ToggleFullScreen,
    Close,
}

/// Which side or corner of the window a resize drag pulls.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum WindowEdge {
    North,
    South,
    East,
    West,
    NorthEast,
    NorthWest,
    SouthEast,
    SouthWest,
}

/// Event emitted by the shared UI contract.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum UiEvent {
    Control { path: String, action: ControlAction },
    SelectPreset(String),
    ToggleModule(String),
    OpenSettings,
    LibraryQuery(String),
    Window(WindowCommand),
}
