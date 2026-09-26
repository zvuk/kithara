use kithara_test_utils::kithara;
use kithara_ui_draw::{Pt, Rect};
use kithara_ui_input::{
    Hit, Input, Modifiers, Outcome, PointerPhase, mouse as mouse_input, recognizers::Crossing,
};

fn hit(x: f32) -> Hit {
    Hit::new(
        Some(Pt { x, y: 10.0 }),
        Rect {
            h: 20.0,
            w: 20.0,
            x: 0.0,
            y: 0.0,
        },
    )
}

fn moved() -> Input<'static> {
    Input::Pointer(mouse_input(PointerPhase::Move, Some(Pt { x: 0.0, y: 0.0 })))
}

#[kithara::test]
fn crossing_observes_each_boundary_once_without_capture() {
    let mut crossing = Crossing::default();

    assert_eq!(crossing.on_input(moved(), &hit(30.0)), Outcome::IGNORED);
    let entered = crossing.on_input(moved(), &hit(10.0));
    assert_eq!(entered, Outcome::observed(true));
    assert!(!entered.is_captured());
    assert_eq!(crossing.on_input(moved(), &hit(11.0)), Outcome::IGNORED);
    assert_eq!(
        crossing.on_input(Input::ModifiersChanged(Modifiers::default()), &hit(11.0),),
        Outcome::IGNORED
    );
    let exited = crossing.on_input(moved(), &hit(30.0));
    assert_eq!(exited, Outcome::observed(false));
    assert!(!exited.is_captured());
    assert_eq!(crossing.on_input(moved(), &hit(31.0)), Outcome::IGNORED);
}

#[kithara::test]
fn leaving_the_pointer_surface_exits_once() {
    let mut crossing = Crossing::default();
    assert_eq!(
        crossing.on_input(moved(), &hit(10.0)),
        Outcome::observed(true)
    );

    assert_eq!(
        crossing.on_input(
            Input::Pointer(mouse_input(PointerPhase::Leave, None)),
            &hit(10.0)
        ),
        Outcome::observed(false)
    );
    assert_eq!(
        crossing.on_input(
            Input::Pointer(mouse_input(PointerPhase::Leave, None)),
            &hit(10.0)
        ),
        Outcome::IGNORED
    );
}
