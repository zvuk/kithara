use kithara_test_utils::kithara;
use kithara_ui_draw::{Pt, Rect};
use kithara_ui_input::{Hit, Input, PointerButton, PointerId, PointerInput, PointerPhase};

#[kithara::test]
fn pointer_input_preserves_identity_button_phase_position_and_clicks() {
    let pointer = PointerInput::new(
        PointerId(17),
        Some(PointerButton::Secondary),
        PointerPhase::DoubleClick,
        Some(Pt { x: 13.0, y: 21.0 }),
        2,
    );

    let Input::Pointer(decoded) = Input::Pointer(pointer) else {
        panic!("pointer input must stay in the neutral pointer vocabulary");
    };
    assert_eq!(decoded, pointer);
}

#[kithara::test]
fn pointer_phase_names_every_required_recognized_stage() {
    assert_eq!(
        [
            PointerPhase::Down,
            PointerPhase::Move,
            PointerPhase::Up,
            PointerPhase::Leave,
            PointerPhase::Cancel,
            PointerPhase::DoubleClick,
            PointerPhase::LongPress,
            PointerPhase::MoveLongPress,
        ]
        .len(),
        8
    );
}

#[kithara::test]
fn hit_keeps_the_unclamped_point_separate_from_containment() {
    let point = Pt { x: 120.0, y: 25.0 };
    let hit = Hit::new(
        Some(point),
        Rect {
            x: 0.0,
            y: 0.0,
            w: 100.0,
            h: 50.0,
        },
    );

    assert_eq!(hit.at(), Some(point));
    assert_eq!(hit.inside(), None);
}
