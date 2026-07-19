use kithara_ui::render::UiEvent;

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
    Modular(UiEvent),
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;
    use kithara_ui::render::{ControlAction, UiEvent};

    use super::Message;

    #[kithara::test]
    fn modular_message_carries_ui_event_directly() {
        let message = Message::Modular(UiEvent::Control {
            path: "deck-a/transport/play".to_owned(),
            action: ControlAction::Activate,
        });

        let Message::Modular(UiEvent::Control { path, action }) = message else {
            panic!("message must preserve the shared UI event");
        };
        assert_eq!(path, "deck-a/transport/play");
        assert_eq!(action, ControlAction::Activate);
    }
}
