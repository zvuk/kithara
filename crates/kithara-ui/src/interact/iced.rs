use iced::{
    Event, Point, Rectangle,
    advanced::{InputMethod as IcedInputMethod, input_method},
    keyboard::{
        self, Event as KeyboardEvent,
        key::{Key as IcedKey, Named},
    },
    mouse::{self, Button, Cursor, ScrollDelta},
};
use input_method::Event as InputMethodEvent;

use super::{
    CursorShape, Hit, Input, InputMethod, InputMethodRequest, Key, Modifiers, PointerPhase, Scroll,
    mouse as mouse_input,
};
use crate::draw::{Pt, Rect};

pub(crate) fn input(event: &Event) -> Option<Input<'_>> {
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

pub(crate) fn input_method(request: Option<InputMethodRequest<'_>>) -> IcedInputMethod<&str> {
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
        purpose: input_method::Purpose::Normal,
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

pub(crate) fn hit(bounds: Rectangle, cursor: Cursor) -> Hit {
    Hit::new(cursor.position().map(Into::into), bounds.into())
}

impl From<Point> for Pt {
    fn from(point: Point) -> Self {
        Self {
            x: point.x,
            y: point.y,
        }
    }
}

impl From<Rectangle> for Rect {
    fn from(rectangle: Rectangle) -> Self {
        Self {
            h: rectangle.height,
            w: rectangle.width,
            x: rectangle.x,
            y: rectangle.y,
        }
    }
}

impl From<CursorShape> for mouse::Interaction {
    fn from(shape: CursorShape) -> Self {
        match shape {
            CursorShape::None => Self::None,
            CursorShape::Grab => Self::Grab,
            CursorShape::Grabbing => Self::Grabbing,
            CursorShape::Pointer => Self::Pointer,
            CursorShape::ResizeDiagonalDown => Self::ResizingDiagonallyDown,
            CursorShape::ResizeDiagonalUp => Self::ResizingDiagonallyUp,
            CursorShape::ResizeH => Self::ResizingHorizontally,
            CursorShape::ResizeV => Self::ResizingVertically,
            CursorShape::Text => Self::Text,
        }
    }
}

#[cfg(test)]
mod tests {
    use iced::keyboard::{
        self, Location, Modifiers as IcedModifiers,
        key::{Named, Physical},
    };
    use kithara_test_utils::kithara;

    use super::*;
    use crate::interact::{MOUSE, PointerButton, PointerInput};

    #[kithara::test]
    fn key_press_and_release_preserve_key_and_all_modifiers() {
        let pressed_modifiers = IcedModifiers::ALT | IcedModifiers::LOGO;
        let released_modifiers = IcedModifiers::CTRL | IcedModifiers::SHIFT;
        let pressed = Event::Keyboard(keyboard::Event::KeyPressed {
            key: keyboard::Key::Character("z".into()),
            modified_key: keyboard::Key::Character("Z".into()),
            physical_key: Physical::Code(keyboard::key::Code::KeyZ),
            location: Location::Standard,
            modifiers: pressed_modifiers,
            text: Some("z".into()),
            repeat: false,
        });
        let released = Event::Keyboard(keyboard::Event::KeyReleased {
            key: keyboard::Key::Named(Named::Delete),
            modified_key: keyboard::Key::Named(Named::Delete),
            physical_key: Physical::Code(keyboard::key::Code::Delete),
            location: Location::Standard,
            modifiers: released_modifiers,
        });

        assert!(matches!(
            input(&pressed),
            Some(Input::KeyPressed {
                key: Key::Character("z"),
                modifiers: decoded,
                text: Some("z"),
            }) if decoded == Modifiers::new(true, false, true, false)
        ));
        assert!(matches!(
            input(&released),
            Some(Input::KeyReleased {
                key: Key::Delete,
                modifiers: decoded,
            }) if decoded == Modifiers::new(false, true, false, true)
        ));
    }

    #[kithara::test]
    fn input_method_events_preserve_composition_and_byte_selection() {
        let events = [
            Event::InputMethod(InputMethodEvent::Opened),
            Event::InputMethod(InputMethodEvent::Preedit("かな".to_owned(), Some(3..6))),
            Event::InputMethod(InputMethodEvent::Commit("仮名".to_owned())),
            Event::InputMethod(InputMethodEvent::Closed),
        ];

        assert!(matches!(
            input(&events[0]),
            Some(Input::InputMethod(InputMethod::Opened))
        ));
        assert!(matches!(
            input(&events[1]),
            Some(Input::InputMethod(InputMethod::Preedit {
                content: "かな",
                selection: Some((3, 6)),
            }))
        ));
        assert!(matches!(
            input(&events[2]),
            Some(Input::InputMethod(InputMethod::Commit("仮名")))
        ));
        assert!(matches!(
            input(&events[3]),
            Some(Input::InputMethod(InputMethod::Closed))
        ));
    }

    #[kithara::test]
    fn a_modifiers_change_is_portable_input() {
        let changed = Event::Keyboard(keyboard::Event::ModifiersChanged(IcedModifiers::SHIFT));

        let Some(Input::ModifiersChanged(modifiers)) = input(&changed) else {
            panic!("the retained hero wave needs the current modifiers before it sees a press");
        };
        assert!(modifiers.shift());
    }

    #[kithara::test]
    fn cursor_left_decodes_without_inventing_a_position() {
        assert!(matches!(
            input(&Event::Mouse(mouse::Event::CursorLeft)),
            Some(Input::Pointer(PointerInput {
                id: MOUSE,
                button: None,
                phase: PointerPhase::Leave,
                at: None,
                clicks: 1,
            }))
        ));
    }

    #[kithara::test]
    fn mouse_events_share_stable_identity_and_one_click_count() {
        let events = [
            Event::Mouse(mouse::Event::ButtonPressed(Button::Left)),
            Event::Mouse(mouse::Event::CursorMoved {
                position: Point::new(9.0, 14.0),
            }),
            Event::Mouse(mouse::Event::ButtonReleased(Button::Left)),
        ];
        let expected = [
            (PointerPhase::Down, Some(PointerButton::Primary), None),
            (PointerPhase::Move, None, Some(Pt { x: 9.0, y: 14.0 })),
            (PointerPhase::Up, Some(PointerButton::Primary), None),
        ];

        for (event, (phase, button, at)) in events.iter().zip(expected) {
            let Some(Input::Pointer(pointer)) = input(event) else {
                panic!("mouse event must decode as neutral pointer input");
            };
            assert_eq!(pointer.id, MOUSE);
            assert_eq!(pointer.button, button);
            assert_eq!(pointer.phase, phase);
            assert_eq!(pointer.at, at);
            assert_eq!(pointer.clicks, 1);
        }
    }

    #[kithara::test]
    fn diagonal_cursor_shapes_map_to_their_iced_orientations() {
        assert!(matches!(
            mouse::Interaction::from(CursorShape::ResizeDiagonalDown),
            mouse::Interaction::ResizingDiagonallyDown
        ));
        assert!(matches!(
            mouse::Interaction::from(CursorShape::ResizeDiagonalUp),
            mouse::Interaction::ResizingDiagonallyUp
        ));
    }

    #[kithara::test]
    fn only_the_left_button_arms_a_gesture() {
        for button in [Button::Right, Button::Middle] {
            assert!(
                input(&Event::Mouse(mouse::Event::ButtonPressed(button))).is_none(),
                "{button:?} must stay with the child"
            );
            assert!(
                input(&Event::Mouse(mouse::Event::ButtonReleased(button))).is_none(),
                "{button:?} must stay with the child"
            );
        }
    }

    #[kithara::test]
    fn wheel_decode_preserves_both_axes_and_units() {
        for (delta, expected_x, expected_y, pixels) in [
            (ScrollDelta::Lines { x: 3.0, y: -2.0 }, 3.0, -2.0, false),
            (ScrollDelta::Pixels { x: -7.5, y: 4.25 }, -7.5, 4.25, true),
        ] {
            let Some(Input::Wheel(scroll)) =
                input(&Event::Mouse(mouse::Event::WheelScrolled { delta }))
            else {
                panic!("a mouse wheel must become portable input");
            };

            assert_eq!(scroll.x(), expected_x);
            assert_eq!(scroll.y(), expected_y);
            assert_eq!(scroll.is_pixels(), pixels);
        }
    }
}
