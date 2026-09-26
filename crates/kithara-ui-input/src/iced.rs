use iced::{
    Event, Rectangle,
    advanced::{InputMethod as IcedInputMethod, input_method, input_method::Purpose},
    keyboard::{
        self, Event as KeyboardEvent,
        key::{Key as IcedKey, Named},
    },
    mouse::{self, Button, Cursor, ScrollDelta},
};
use input_method::Event as InputMethodEvent;

use super::{
    Hit, Input, InputMethod, InputMethodRequest, Key, Modifiers, PointerPhase, Scroll,
    mouse as mouse_input,
};

#[must_use]
pub fn input(event: &Event) -> Option<Input<'_>> {
    match event {
        Event::Keyboard(KeyboardEvent::KeyPressed {
            key,
            modifiers,
            text,
            ..
        }) => Some(Input::KeyPressed {
            key: portable_key(key),
            modifiers: portable_modifiers(*modifiers),
            text: text.as_deref(),
        }),
        Event::Keyboard(KeyboardEvent::KeyReleased { key, modifiers, .. }) => {
            Some(Input::KeyReleased {
                key: portable_key(key),
                modifiers: portable_modifiers(*modifiers),
            })
        }
        Event::Keyboard(KeyboardEvent::ModifiersChanged(modifiers)) => {
            Some(Input::ModifiersChanged(portable_modifiers(*modifiers)))
        }
        Event::InputMethod(event) => Some(Input::InputMethod(match event {
            InputMethodEvent::Opened => InputMethod::Opened,
            InputMethodEvent::Preedit(content, selection) => InputMethod::Preedit {
                content,
                selection: selection.as_ref().map(|range| (range.start, range.end)),
            },
            InputMethodEvent::Commit(content) => InputMethod::Commit(content),
            InputMethodEvent::Closed => InputMethod::Closed,
        })),
        Event::Mouse(mouse::Event::ButtonPressed(Button::Left)) => {
            Some(Input::Pointer(mouse_input(PointerPhase::Down, None)))
        }
        Event::Mouse(mouse::Event::CursorMoved { position }) => Some(Input::Pointer(mouse_input(
            PointerPhase::Move,
            Some((*position).into()),
        ))),
        Event::Mouse(mouse::Event::CursorLeft) => {
            Some(Input::Pointer(mouse_input(PointerPhase::Leave, None)))
        }
        Event::Mouse(mouse::Event::ButtonReleased(Button::Left)) => {
            Some(Input::Pointer(mouse_input(PointerPhase::Up, None)))
        }
        Event::Mouse(mouse::Event::WheelScrolled { delta }) => Some(Input::Wheel(match delta {
            ScrollDelta::Lines { x, y } => Scroll::Lines { x: *x, y: *y },
            ScrollDelta::Pixels { x, y } => Scroll::Pixels { x: *x, y: *y },
        })),
        _ => None,
    }
}

fn portable_key<'a>(key: &'a IcedKey<impl AsRef<str>>) -> Key<'a> {
    match key {
        IcedKey::Named(Named::ArrowDown) => Key::ArrowDown,
        IcedKey::Named(Named::ArrowLeft) => Key::ArrowLeft,
        IcedKey::Named(Named::ArrowRight) => Key::ArrowRight,
        IcedKey::Named(Named::ArrowUp) => Key::ArrowUp,
        IcedKey::Named(Named::Backspace) => Key::Backspace,
        IcedKey::Named(Named::Delete) => Key::Delete,
        IcedKey::Named(Named::End) => Key::End,
        IcedKey::Named(Named::Enter) => Key::Enter,
        IcedKey::Named(Named::Escape) => Key::Escape,
        IcedKey::Named(Named::Home) => Key::Home,
        IcedKey::Named(Named::Space) => Key::Space,
        IcedKey::Character(character) => Key::Character(character.as_ref()),
        IcedKey::Named(_) | IcedKey::Unidentified => Key::Other,
    }
}

#[must_use]
pub fn input_method(request: Option<InputMethodRequest<'_>>) -> IcedInputMethod<&str> {
    let Some(request) = request else {
        return IcedInputMethod::Disabled;
    };
    IcedInputMethod::Enabled {
        cursor: Rectangle {
            height: request.caret.h,
            width: request.caret.w,
            x: request.caret.x,
            y: request.caret.y,
        },
        purpose: Purpose::Normal,
        preedit: request.preedit.map(|preedit| input_method::Preedit {
            content: preedit.content,
            selection: preedit.selection,
            text_size: Some(iced::Pixels(request.text_size)),
        }),
    }
}

fn portable_modifiers(modifiers: keyboard::Modifiers) -> Modifiers {
    Modifiers::new(
        modifiers.alt(),
        modifiers.control(),
        modifiers.logo(),
        modifiers.shift(),
    )
}

pub fn hit(bounds: Rectangle, cursor: Cursor) -> Hit {
    Hit::new(cursor.position().map(Into::into), bounds.into())
}
