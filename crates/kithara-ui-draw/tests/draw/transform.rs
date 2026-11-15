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

#[kithara::test]
#[case::identity(Transform::IDENTITY, true)]
#[case::moved(Transform::translate(Pt { x: 40.0, y: -3.0 }), true)]
#[case::turned(Transform::rotate(0.5), true)]
#[case::squashed(Transform::scale(Pt { x: 3.0, y: 0.25 }), true)]
#[case::flattened_onto_a_line(Transform::scale(Pt { x: 0.0, y: 1.0 }), false)]
#[case::collapsed_to_a_point(Transform::scale(Pt { x: 0.0, y: 0.0 }), false)]
#[case::sheared_flat(Transform { xx: 1.0, xy: 2.0, yx: 0.5, yy: 1.0, dx: 0.0, dy: 0.0 }, false)]
#[case::not_a_number(Transform::scale(Pt { x: f32::NAN, y: 1.0 }), false)]
#[case::thrown_to_infinity(Transform::translate(Pt { x: f32::INFINITY, y: 0.0 }), false)]
fn only_a_transform_that_loses_no_point_is_invertible(
    #[case] transform: Transform,
    #[case] invertible: bool,
) {
    assert_eq!(transform.is_invertible(), invertible);
}
