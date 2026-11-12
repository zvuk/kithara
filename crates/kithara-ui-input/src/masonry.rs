use std::hash::{Hash, Hasher};

use masonry::{
    core::{CursorIcon, Ime, TextEvent},
    dpi::PhysicalPosition,
    ui_events::{
        ScrollDelta,
        keyboard::{
            Code, Key as MasonryKey, KeyState, KeyboardEvent, Modifiers as MasonryModifiers,
            NamedKey,
        },
        pointer::{
            PointerButton as MasonryPointerButton, PointerButtonEvent, PointerEvent,
            PointerGestureEvent, PointerId as MasonryPointerId, PointerInfo, PointerScrollEvent,
            PointerUpdate,
        },
    },
};
use num_traits::cast::AsPrimitive;

use super::{
    CursorShape, Input, InputMethod, Key, MOUSE, Modifiers, PointerButton, PointerId, Scroll,
};

mod consts {
    use super::*;

    pub(super) const NAMED_KEYS: [(NamedKey, Key<'static>); 10] = [
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

    pub(super) const LETTERS: [(Code, char); 26] = crate::letters!(Code);
}

#[must_use]
pub fn portable_text_input(event: &TextEvent) -> Option<Input<'_>> {
    match event {
        TextEvent::Keyboard(event) => {
            let key = portable_key(&event.key, event.code);
            let modifiers = portable_modifiers(event.modifiers);
            if event.state.is_down() {
                let text = if event.is_composing {
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

#[must_use]
pub fn masonry_text_event(input: Input<'_>) -> Option<TextEvent> {
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

/// Whether the event only says which modifiers are held, the way
/// [`masonry_text_event`] carries a modifier change.
#[must_use]
pub fn changes_modifiers(event: &TextEvent) -> bool {
    let TextEvent::Keyboard(keyboard) = event else {
        return false;
    };
    let held = Input::ModifiersChanged(portable_modifiers(keyboard.modifiers));
    masonry_text_event(held).as_ref() == Some(event)
}

fn keyboard_event(state: KeyState, key: Key<'_>, modifiers: Modifiers) -> TextEvent {
    TextEvent::Keyboard(KeyboardEvent {
        state,
        key: masonry_key(key),
        code: masonry_code(key),
        modifiers: masonry_modifiers(modifiers),
        ..KeyboardEvent::default()
    })
}

fn portable_key(key: &MasonryKey, code: Code) -> Key<'_> {
    match key {
        MasonryKey::Character(character) if character == " " => Key::Space,
        MasonryKey::Character(character) => Key::character(
            character,
            consts::LETTERS
                .iter()
                .find(|(lettered, _)| *lettered == code)
                .map(|(_, letter)| *letter),
        ),
        MasonryKey::Named(named) => {
            let Some((_, neutral)) = consts::NAMED_KEYS
                .iter()
                .find(|(candidate, _)| candidate == named)
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
        Key::Character { text, .. } => MasonryKey::Character(text.to_owned()),
        Key::Other => MasonryKey::Named(NamedKey::Unidentified),
        named => {
            let Some((candidate, _)) = consts::NAMED_KEYS
                .iter()
                .find(|(_, neutral)| *neutral == named)
            else {
                return MasonryKey::Named(NamedKey::Unidentified);
            };
            MasonryKey::Named(*candidate)
        }
    }
}

/// The physical key a neutral key names for shortcuts, so the key survives the
/// trip through masonry's event and back.
fn masonry_code(key: Key<'_>) -> Code {
    let Key::Character {
        latin: Some(latin), ..
    } = key
    else {
        return Code::Unidentified;
    };
    consts::LETTERS
        .iter()
        .find(|(_, letter)| letter.eq_ignore_ascii_case(&latin))
        .map_or(Code::Unidentified, |(code, _)| *code)
}

#[must_use]
pub fn portable_modifiers(modifiers: MasonryModifiers) -> Modifiers {
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
#[must_use]
pub const fn pointer_position(event: &PointerEvent) -> Option<PhysicalPosition<f64>> {
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
#[must_use]
pub fn portable_scroll(delta: ScrollDelta, scale: f64) -> Option<Scroll> {
    match delta {
        ScrollDelta::LineDelta(x, y) => Some(Scroll::Lines { x, y }),
        ScrollDelta::PixelDelta(delta) => Some(Scroll::Pixels {
            x: AsPrimitive::<f32>::as_(delta.x / scale),
            y: AsPrimitive::<f32>::as_(delta.y / scale),
        }),
        ScrollDelta::PageDelta(_, _) => None,
    }
}

/// The neutral identity of the pointer that produced `event`.
///
/// Masonry numbers pointers from one, its primary pointer being one; the
/// neutral model numbers them from zero, [`MOUSE`] being zero. An event that
/// names no pointer is the mouse's.
#[must_use]
pub fn portable_pointer_id(event: &PointerEvent) -> PointerId {
    let (PointerEvent::Down(PointerButtonEvent { pointer, .. })
    | PointerEvent::Up(PointerButtonEvent { pointer, .. })
    | PointerEvent::Move(PointerUpdate { pointer, .. })
    | PointerEvent::Cancel(pointer)
    | PointerEvent::Enter(pointer)
    | PointerEvent::Leave(pointer)
    | PointerEvent::Scroll(PointerScrollEvent { pointer, .. })
    | PointerEvent::Gesture(PointerGestureEvent { pointer, .. })) = event;
    let PointerInfo {
        pointer_id: Some(id),
        ..
    } = pointer
    else {
        return MOUSE;
    };
    let mut number = PointerNumber(0);
    id.hash(&mut number);
    PointerId(number.0.saturating_sub(1))
}

/// The masonry identity of a neutral pointer, inverse to
/// [`portable_pointer_id`]; `None` only for the one neutral number masonry
/// cannot spell.
#[must_use]
pub fn masonry_pointer_id(id: PointerId) -> Option<MasonryPointerId> {
    MasonryPointerId::new(id.0.checked_add(1)?)
}

/// Reads the number a masonry pointer id carries.
///
/// ui-events 0.2 keeps that number private and offers no accessor, but the
/// id's derived `Hash` writes exactly it, once, as a `u64`; this hasher keeps
/// what it is handed instead of mixing it.
struct PointerNumber(u64);

impl Hasher for PointerNumber {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, _bytes: &[u8]) {}

    fn write_u64(&mut self, number: u64) {
        self.0 = number;
    }
}

#[must_use]
pub fn pointer_button(button: MasonryPointerButton) -> PointerButton {
    match button {
        MasonryPointerButton::Primary => PointerButton::Primary,
        MasonryPointerButton::Secondary => PointerButton::Secondary,
        MasonryPointerButton::Auxiliary => PointerButton::Auxiliary,
        MasonryPointerButton::X1 => PointerButton::Back,
        MasonryPointerButton::X2 => PointerButton::Forward,
        button => PointerButton::Other(button as u32),
    }
}

/// The cursor masonry shows for a neutral cursor shape.
#[must_use]
pub const fn cursor_icon(shape: CursorShape) -> CursorIcon {
    match shape {
        CursorShape::None => CursorIcon::Default,
        CursorShape::Grab => CursorIcon::Grab,
        CursorShape::Grabbing => CursorIcon::Grabbing,
        CursorShape::Pointer => CursorIcon::Pointer,
        CursorShape::ResizeDiagonalDown => CursorIcon::NwseResize,
        CursorShape::ResizeDiagonalUp => CursorIcon::NeswResize,
        CursorShape::ResizeH => CursorIcon::EwResize,
        CursorShape::ResizeV => CursorIcon::NsResize,
        CursorShape::Text => CursorIcon::Text,
    }
}
