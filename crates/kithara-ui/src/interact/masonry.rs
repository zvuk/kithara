//! Translates pointer, keyboard, IME, and modifier events between Masonry and
//! the neutral input contract, so no widget has to speak the toolkit's dialect.

use masonry::{
    core::{Ime, TextEvent},
    dpi::PhysicalPosition,
    ui_events::{
        ScrollDelta,
        keyboard::{
            Key as MasonryKey, KeyState, KeyboardEvent, Modifiers as MasonryModifiers, NamedKey,
        },
        pointer::{PointerButton as MasonryPointerButton, PointerEvent},
    },
};

use super::{Input, InputMethod, Key, Modifiers, PointerButton, Scroll};

const NAMED_KEYS: [(NamedKey, Key<'static>); 10] = [
    (NamedKey::ArrowDown, Key::ArrowDown),
    (NamedKey::ArrowLeft, Key::ArrowLeft),
    (NamedKey::ArrowRight, Key::ArrowRight),
    (NamedKey::ArrowUp, Key::ArrowUp),
    (NamedKey::Backspace, Key::Backspace),
    (NamedKey::Delete, Key::Delete),
    (NamedKey::End, Key::End),
    (NamedKey::Enter, Key::Enter),
    (NamedKey::Escape, Key::Escape),
    (NamedKey::Home, Key::Home),
];

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
                        MasonryKey::Character(text) => Some(text.as_str()),
                        MasonryKey::Named(_) => None,
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

pub(crate) fn masonry_text_event(input: Input<'_>) -> Option<TextEvent> {
    match input {
        Input::KeyPressed { key, modifiers, .. } => {
            Some(keyboard_event(KeyState::Down, key, modifiers))
        }
        Input::KeyReleased { key, modifiers } => Some(keyboard_event(KeyState::Up, key, modifiers)),
        Input::ModifiersChanged(modifiers) => {
            Some(keyboard_event(KeyState::Up, Key::Other, modifiers))
        }
        Input::InputMethod(event) => Some(TextEvent::Ime(match event {
            InputMethod::Opened => Ime::Enabled,
            InputMethod::Preedit { content, selection } => {
                Ime::Preedit(content.to_owned(), selection)
            }
            InputMethod::Commit(content) => Ime::Commit(content.to_owned()),
            InputMethod::Closed => Ime::Disabled,
        })),
        Input::Pointer(_) | Input::Wheel(_) => None,
    }
}

fn keyboard_event(state: KeyState, key: Key<'_>, modifiers: Modifiers) -> TextEvent {
    TextEvent::Keyboard(KeyboardEvent {
        state,
        key: masonry_key(key),
        modifiers: masonry_modifiers(modifiers),
        ..KeyboardEvent::default()
    })
}

fn portable_key(key: &MasonryKey) -> Key<'_> {
    match key {
        MasonryKey::Character(character) if character == " " => Key::Space,
        MasonryKey::Character(character) => Key::Character(character),
        MasonryKey::Named(named) => {
            let Some((_, neutral)) = NAMED_KEYS.iter().find(|(candidate, _)| candidate == named)
            else {
                return Key::Other;
            };
            *neutral
        }
    }
}

fn masonry_key(key: Key<'_>) -> MasonryKey {
    match key {
        Key::Space => MasonryKey::Character(" ".to_owned()),
        Key::Character(text) => MasonryKey::Character(text.to_owned()),
        Key::Other => MasonryKey::Named(NamedKey::Unidentified),
        named => {
            let Some((candidate, _)) = NAMED_KEYS.iter().find(|(_, neutral)| *neutral == named)
            else {
                return MasonryKey::Named(NamedKey::Unidentified);
            };
            MasonryKey::Named(*candidate)
        }
    }
}

pub(crate) fn portable_modifiers(modifiers: MasonryModifiers) -> Modifiers {
    Modifiers::new(
        modifiers.alt(),
        modifiers.ctrl(),
        modifiers.meta(),
        modifiers.shift(),
    )
}

fn masonry_modifiers(modifiers: Modifiers) -> MasonryModifiers {
    let mut out = MasonryModifiers::empty();
    if modifiers.alt() {
        out |= MasonryModifiers::ALT;
    }
    if modifiers.control() {
        out |= MasonryModifiers::CONTROL;
    }
    if modifiers.logo() {
        out |= MasonryModifiers::META;
    }
    if modifiers.shift() {
        out |= MasonryModifiers::SHIFT;
    }
    out
}

/// Where a pointer event happened, in the window's own pixels.
///
/// Enter, leave and cancel carry no position: a widget being told the pointer
/// left says nothing about where it went, and answering with a stale point
/// would put the gesture somewhere the hand is not.
pub(crate) const fn pointer_position(event: &PointerEvent) -> Option<PhysicalPosition<f64>> {
    match event {
        PointerEvent::Down(button) | PointerEvent::Up(button) => Some(button.state.position),
        PointerEvent::Move(update) => Some(update.current.position),
        PointerEvent::Scroll(scroll) => Some(scroll.state.position),
        PointerEvent::Gesture(gesture) => Some(gesture.state.position),
        PointerEvent::Cancel(_) | PointerEvent::Enter(_) | PointerEvent::Leave(_) => None,
    }
}

/// One wheel delta in the neutral vocabulary, at the host's logical scale.
///
/// A page delta has no neutral spelling — the recognizers count lines and
/// pixels — so it is declined rather than guessed at.
pub(crate) fn portable_scroll(delta: ScrollDelta, scale: f64) -> Option<Scroll> {
    match delta {
        ScrollDelta::LineDelta(x, y) => Some(Scroll::Lines { x, y }),
        ScrollDelta::PixelDelta(delta) => Some(Scroll::Pixels {
            x: num_traits::cast::AsPrimitive::<f32>::as_(delta.x / scale),
            y: num_traits::cast::AsPrimitive::<f32>::as_(delta.y / scale),
        }),
        ScrollDelta::PageDelta(_, _) => None,
    }
}

pub(crate) fn pointer_button(button: MasonryPointerButton) -> PointerButton {
    match button {
        MasonryPointerButton::Primary => PointerButton::Primary,
        MasonryPointerButton::Secondary => PointerButton::Secondary,
        MasonryPointerButton::Auxiliary => PointerButton::Auxiliary,
        MasonryPointerButton::X1 => PointerButton::Back,
        MasonryPointerButton::X2 => PointerButton::Forward,
        button => PointerButton::Other(button as u32),
    }
}
