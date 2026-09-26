use kithara_test_utils::kithara;
use kithara_ui_draw::{LineCap, LineJoin, Pen};

/// A bare width is a pen, so a caller that has never heard of caps keeps
/// passing widths.
#[kithara::test]
fn a_width_is_a_pen() {
    assert_eq!(Pen::from(1.5), Pen::new(1.5));
    assert_eq!(Pen::new(1.5).cap, LineCap::Butt);
    assert_eq!(Pen::new(1.5).join, LineJoin::Miter);
}

#[kithara::test]
fn shaping_one_end_leaves_the_rest_of_the_pen_alone() {
    let pen = Pen::new(2.0)
        .with_cap(LineCap::Round)
        .with_join(LineJoin::Bevel);

    assert_eq!(pen.cap, LineCap::Round);
    assert_eq!(pen.join, LineJoin::Bevel);
    assert_eq!(pen.width, 2.0);
}
