use kithara_test_utils::kithara;
use kithara_ui_draw::{Pt, Transform};

#[kithara::test]
fn composing_reads_in_the_order_a_point_travels() {
    let turn = Transform::rotate(core::f32::consts::FRAC_PI_2);
    let shift = Transform::translate(Pt { x: 10.0, y: 0.0 });

    let landed = turn.then(shift).apply(Pt { x: 1.0, y: 0.0 });

    assert_eq!(
        Pt {
            x: landed.x.round(),
            y: landed.y.round(),
        },
        Pt { x: 10.0, y: 1.0 }
    );
}

/// The other order is a different transform, which is the whole reason the
/// order is written down rather than left to a matrix convention.
#[kithara::test]
fn the_other_order_lands_somewhere_else() {
    let turn = Transform::rotate(core::f32::consts::FRAC_PI_2);
    let shift = Transform::translate(Pt { x: 10.0, y: 0.0 });

    let landed = shift.then(turn).apply(Pt { x: 1.0, y: 0.0 });

    assert_eq!(
        Pt {
            x: landed.x.round(),
            y: landed.y.round(),
        },
        Pt { x: 0.0, y: 11.0 }
    );
}

#[kithara::test]
fn the_identity_is_what_default_gives() {
    assert!(Transform::default().is_identity());
}
