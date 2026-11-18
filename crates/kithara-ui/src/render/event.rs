use std::collections::BTreeMap;

use crate::{draw::Pt, validate::Gesture};

/// Action emitted by an interactive control.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum ControlAction {
    Activate,
    SecondaryActivate,
    /// A double click, emitted after its second press.
    DoubleActivate,
    SetScalar(f64),
    /// Where a placement the pointer carried has come to rest, in the box of
    /// the stage that holds it.
    Place(Pt),
    StepScalar(f32),
    SelectIndex(usize),
    Text(String),
    Record(BTreeMap<String, String>),
}

impl ControlAction {
    pub(crate) const fn gesture(&self) -> Gesture {
        match self {
            Self::Activate => Gesture::Press,
            Self::SecondaryActivate => Gesture::Secondary,
            Self::DoubleActivate => Gesture::Double,
            Self::SetScalar(_) => Gesture::Scalar,
            Self::StepScalar(_) => Gesture::Step,
            Self::SelectIndex(_) => Gesture::Index,
            Self::Text(_) => Gesture::Text,
            Self::Record(_) => Gesture::Record,
            Self::Place(_) => Gesture::Place,
        }
    }
}

/// The value one declared write carries, typed by the endpoint it addresses.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum WriteValue {
    Trigger,
    Scalar(f64),
    Step(f32),
    Index(usize),
    Text(String),
    Record(BTreeMap<String, String>),
    Point(Pt),
    Range(f64, f64),
}

/// One step of a row carried from the table it was picked up in to the drop
/// zone it is let go over, which the toolkit follows for itself.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum Carry {
    /// The row at this index is now being carried out of the table.
    Start(usize),
    /// The pointer crossed into (`true`) or out of (`false`) a drop zone.
    Over(bool),
    /// The pointer was released and the carry ended.
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

/// What a document publishes, for its host to settle through the compiled
/// screen it was drawn from.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum Published {
    /// A hand operated the control at `path`.
    Gesture { path: String, action: ControlAction },
    /// A row is being carried, which the toolkit follows on its own.
    Carry { path: String, step: Carry },
    /// An event that needs no settling: window chrome, or what a widget the
    /// host registered maps its own action to.
    Host(UiEvent),
}

impl Published {
    /// What a piece of window chrome asks of the window.
    #[must_use]
    pub const fn window(command: WindowCommand) -> Self {
        Self::Host(UiEvent::Window(command))
    }
}

/// What a host is handed once a document's publication settled.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum UiEvent {
    /// A write the document declares: the scoped endpoint key and its value.
    Write {
        key: String,
        value: WriteValue,
    },
    Window(WindowCommand),
}
