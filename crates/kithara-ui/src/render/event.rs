/// Action emitted by an interactive control.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum ControlAction {
    Activate,
    SetScalar(f64),
    SelectIndex(usize),
}

/// Event emitted by the shared UI contract.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum UiEvent {
    Control { path: String, action: ControlAction },
    SelectPreset(String),
    ToggleModule(String),
    OpenSettings,
    CloseSettings,
    LibraryQuery(String),
}
