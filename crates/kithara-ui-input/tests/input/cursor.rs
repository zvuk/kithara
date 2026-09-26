use kithara_test_utils::kithara;
use kithara_ui_draw::{Pt, Rect};
use kithara_ui_input::{CursorShape, Hit, Hover};

fn hit(at: Option<Pt>) -> Hit {
    Hit::new(
        at,
        Rect {
            h: 34.0,
            w: 34.0,
            x: 0.0,
            y: 0.0,
        },
    )
}

#[kithara::test]
fn the_cursor_shape_follows_hover_or_an_active_gesture() {
    let hover = Hover::new(CursorShape::ResizeV);

    assert_eq!(
        hover.cursor(false, &hit(Some(Pt { x: 17.0, y: 17.0 }))),
        CursorShape::ResizeV
    );
    assert_eq!(
        hover.cursor(false, &hit(Some(Pt { x: 200.0, y: 200.0 }))),
        CursorShape::None
    );
    assert_eq!(
        hover.cursor(true, &hit(Some(Pt { x: 200.0, y: 200.0 }))),
        CursorShape::ResizeV,
        "an active gesture keeps its shape once the pointer leaves"
    );
}
