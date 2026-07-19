#[derive(Clone, Debug)]
pub(crate) enum ModularMsg {
    SelectPreset(String),
    LibraryQueryChanged(String),
    ToggleModule(String),
    OpenSettings,
    CloseSettings,
    Control { path: String, action: ControlAction },
}

#[derive(Clone, Debug)]
pub(crate) enum ControlAction {
    Activate,
    SetScalar(f64),
    SelectIndex(usize),
}
