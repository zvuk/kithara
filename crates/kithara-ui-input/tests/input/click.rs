use kithara_test_utils::kithara;
use kithara_ui_draw::{Pt, Rect};
use kithara_ui_input::{
    Hit, Input, Outcome, PointerPhase, Scroll, mouse as mouse_input, recognizers::click::on_input,
};

fn at(x: f32) -> Hit {
    Hit::new(
        Some(Pt { x, y: 13.0 }),
        Rect {
            h: 18.0,
            w: 18.0,
            x: 4.0,
            y: 4.0,
        },
    )
}

#[kithara::test]
fn a_press_on_the_control_acts_and_takes_the_pointer() {
    assert_eq!(
        on_input(
            Input::Pointer(mouse_input(PointerPhase::Down, None)),
            &at(13.0)
        ),
        Outcome::set(())
    );
}

#[kithara::test]
fn a_press_beside_the_control_is_left_to_whoever_is_behind() {
    assert_eq!(
        on_input(
            Input::Pointer(mouse_input(PointerPhase::Down, None)),
            &at(40.0)
        ),
        Outcome::IGNORED
    );
}

#[kithara::test]
fn nothing_but_a_press_acts() {
    let over = at(13.0);

    for input in [
        Input::Pointer(mouse_input(
            PointerPhase::Move,
            Some(Pt { x: 13.0, y: 13.0 }),
        )),
        Input::Pointer(mouse_input(PointerPhase::Up, None)),
        Input::Wheel(Scroll::lines(1.0)),
    ] {
        assert_eq!(
            on_input(input, &over),
            Outcome::IGNORED,
            "a control that only clicks must not eat the rest of the gesture"
        );
    }
}
