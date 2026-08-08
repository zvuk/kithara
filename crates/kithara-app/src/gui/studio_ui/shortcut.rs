use iced::{
    Event,
    event::Status,
    keyboard::{Event as KeyboardEvent, Key, key::Named},
};

pub(in crate::gui) fn deletes_focused_track(event: &Event, status: Status) -> bool {
    matches!(
        event,
        Event::Keyboard(KeyboardEvent::KeyPressed {
            key: Key::Named(Named::Delete | Named::Backspace),
            ..
        })
    ) && status == Status::Ignored
}

#[cfg(test)]
mod tests {
    use iced::keyboard::{
        Location, Modifiers,
        key::{Code, Physical},
    };
    use kithara_test_utils::kithara;

    use super::*;

    fn pressed(named: Named) -> Event {
        Event::Keyboard(KeyboardEvent::KeyPressed {
            key: Key::Named(named),
            modified_key: Key::Named(named),
            physical_key: Physical::Code(Code::Delete),
            location: Location::Standard,
            modifiers: Modifiers::empty(),
            text: None,
            repeat: false,
        })
    }

    #[kithara::test]
    fn delete_and_backspace_only_fire_when_the_focused_widget_declined_them() {
        for key in [Named::Delete, Named::Backspace] {
            let event = pressed(key);
            assert!(deletes_focused_track(&event, Status::Ignored));
            assert!(!deletes_focused_track(&event, Status::Captured));
        }
    }

    #[kithara::test]
    fn unrelated_ignored_keys_do_not_delete_the_focused_track() {
        assert!(!deletes_focused_track(
            &pressed(Named::ArrowDown),
            Status::Ignored,
        ));
    }
}
