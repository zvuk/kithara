use kithara_test_utils::kithara;
use kithara_ui_input::{
    MOUSE, PointerId,
    masonry::{masonry_pointer_id, portable_pointer_id},
};
use masonry::ui_events::pointer::{
    PointerEvent, PointerId as MasonryPointerId, PointerInfo, PointerType,
};

fn cancel(pointer_id: Option<MasonryPointerId>) -> PointerEvent {
    PointerEvent::Cancel(PointerInfo {
        pointer_id,
        persistent_device_id: None,
        pointer_type: PointerType::Touch,
    })
}

#[kithara::test]
#[case::primary(1, PointerId(0))]
#[case::second(2, PointerId(1))]
#[case::touch(7, PointerId(6))]
#[case::last(u64::MAX, PointerId(u64::MAX - 1))]
fn a_masonry_pointer_keeps_its_identity_both_ways(#[case] number: u64, #[case] neutral: PointerId) {
    let id = MasonryPointerId::new(number);

    assert_eq!(portable_pointer_id(&cancel(id)), neutral);
    assert_eq!(masonry_pointer_id(neutral), id);
}

#[kithara::test]
fn the_primary_pointer_and_an_unnamed_one_are_the_mouse() {
    assert_eq!(
        portable_pointer_id(&cancel(Some(MasonryPointerId::PRIMARY))),
        MOUSE
    );
    assert_eq!(portable_pointer_id(&cancel(None)), MOUSE);
    assert_eq!(masonry_pointer_id(MOUSE), Some(MasonryPointerId::PRIMARY));
}

#[kithara::test]
fn the_one_neutral_number_masonry_cannot_spell_has_no_masonry_identity() {
    assert_eq!(masonry_pointer_id(PointerId(u64::MAX)), None);
}
