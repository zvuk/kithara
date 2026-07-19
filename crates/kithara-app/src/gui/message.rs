/// All GUI events flow through this enum.
#[derive(Debug, Clone)]
pub(crate) enum Message {
    /// Periodic tick from the subscription.
    Tick,
    /// System close button on a window.
    WindowCloseRequested(iced::window::Id),
    /// Delete the selected track, or the current track when none is selected.
    DeleteTrack,
    /// Modular UI control event.
    Modular(super::modular::ModularMsg),
}
