//! Translating Masonry's keyboard and text events into the neutral input
//! contract, so no widget has to speak the toolkit's dialect.

use masonry::core::{Ime, TextEvent};

use super::{Input, InputMethod, Key, Modifiers};

pub(crate) fn portable_text_input(event: &TextEvent) -> Option<Input<'_>> {
    match event {
        TextEvent::Keyboard(event) => {
            let key = portable_key(&event.key);
            let modifiers = portable_modifiers(event.modifiers);
            if event.state.is_down() {
                let text = if event.is_composing || event.modifiers.ctrl() || event.modifiers.meta()
                {
                    None
                } else {
                    match &event.key {
                        masonry::ui_events::keyboard::Key::Character(text) => Some(text.as_str()),
                        masonry::ui_events::keyboard::Key::Named(_) => None,
                    }
                };
                Some(Input::KeyPressed {
                    key,
                    modifiers,
                    text,
                })
            } else {
                Some(Input::KeyReleased { key, modifiers })
            }
        }
        TextEvent::Ime(event) => Some(Input::InputMethod(match event {
            Ime::Enabled => InputMethod::Opened,
            Ime::Preedit(content, selection) => InputMethod::Preedit {
                content,
                selection: *selection,
            },
            Ime::Commit(content) => InputMethod::Commit(content),
            Ime::Disabled => InputMethod::Closed,
        })),
        TextEvent::WindowFocusChange(_) | TextEvent::ClipboardPaste(_) => None,
    }
}

fn portable_key(key: &masonry::ui_events::keyboard::Key) -> Key<'_> {
    use masonry::ui_events::keyboard::{Key as MasonryKey, NamedKey};

    match key {
        MasonryKey::Named(NamedKey::ArrowDown) => Key::ArrowDown,
        MasonryKey::Named(NamedKey::ArrowLeft) => Key::ArrowLeft,
        MasonryKey::Named(NamedKey::ArrowRight) => Key::ArrowRight,
        MasonryKey::Named(NamedKey::ArrowUp) => Key::ArrowUp,
        MasonryKey::Named(NamedKey::Backspace) => Key::Backspace,
        MasonryKey::Named(NamedKey::Delete) => Key::Delete,
        MasonryKey::Named(NamedKey::End) => Key::End,
        MasonryKey::Named(NamedKey::Enter) => Key::Enter,
        MasonryKey::Named(NamedKey::Escape) => Key::Escape,
        MasonryKey::Named(NamedKey::Home) => Key::Home,
        MasonryKey::Character(character) if character == " " => Key::Space,
        MasonryKey::Character(character) => Key::Character(character),
        MasonryKey::Named(_) => Key::Other,
    }
}

pub(crate) fn portable_modifiers(modifiers: masonry::ui_events::keyboard::Modifiers) -> Modifiers {
    Modifiers::new(
        modifiers.alt(),
        modifiers.ctrl(),
        modifiers.meta(),
        modifiers.shift(),
    )
}
