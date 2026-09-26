use kithara_test_utils::kithara;
use kithara_ui_draw::{FillRule, Outline, Path, Pt, Rect, Verb};

/// The unit square lands exactly on the box it was placed in, so art drawn
/// corner to corner still touches the corners at any size.
#[kithara::test]
fn an_outline_placed_in_a_box_spans_it() {
    let outline = Outline::new(Path::new(
        FillRule::NonZero,
        vec![
            Verb::MoveTo(Pt { x: 0.0, y: 0.0 }),
            Verb::LineTo(Pt { x: 1.0, y: 1.0 }),
            Verb::Close,
        ],
    ));

    let placed = outline.placed(Rect {
        h: 8.0,
        w: 16.0,
        x: 3.0,
        y: 5.0,
    });

    assert_eq!(
        placed.verbs(),
        [
            Verb::MoveTo(Pt { x: 3.0, y: 5.0 }),
            Verb::LineTo(Pt { x: 19.0, y: 13.0 }),
            Verb::Close,
        ]
    );
    assert_eq!(placed.rule(), FillRule::NonZero);
}
