use iced::Element;

/// Shared view contract: a built control renders itself into the event tree.
pub(crate) trait Widget<'a> {
    fn view(self) -> Element<'a, UiEvent>;
}

/// Action emitted by an interactive control.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum ControlAction {
    Activate,
    SetScalar(f64),
    SelectIndex(usize),
}

/// Command emitted by portable window-chrome controls and executed by the host.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum WindowCommand {
    Drag,
    Minimize,
    ToggleMaximize,
    Close,
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
    SettingsShowLayout,
    SettingsShowModules,
    SettingsSelectPreset(String),
    SettingsToggleModule(String),
    SettingsReset,
    SettingsDone,
    LibraryQuery(String),
    Window(WindowCommand),
}
