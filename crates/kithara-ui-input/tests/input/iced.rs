use iced::{
    Event, Point,
    advanced::input_method::Event as InputMethodEvent,
    keyboard::{
        self, Location, Modifiers as IcedModifiers,
        key::{Named, Physical},
    },
    mouse::{self, Button, ScrollDelta},
};
use kithara_test_utils::kithara;
use kithara_ui_draw::Pt;
use kithara_ui_input::{
    CursorShape, Input, InputMethod, Key, MOUSE, Modifiers, PointerButton, PointerInput,
    PointerPhase, ScrollAxis, iced::input,
};

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

        assert_eq!(scroll.delta(ScrollAxis::Horizontal), expected_x);
        assert_eq!(scroll.y(), expected_y);
        assert_eq!(scroll.is_pixels(), pixels);
    }
}
