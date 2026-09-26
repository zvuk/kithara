use kithara_test_utils::kithara;
use kithara_ui_draw::{Pt, Rect};
use kithara_ui_input::{
    CursorShape, Hit, Hover, Input, PointerInput, PointerPhase, mouse,
    recognizers::{Edge, Span, SpanState},
};

const AREA: Rect = Rect {
    h: 20.0,
    w: 200.0,
    x: 10.0,
    y: 0.0,
};

fn span() -> Span {
    Span::new(Hover::new(CursorShape::ResizeH), 0.2, 0.8)
}

fn hit(x: f32) -> Hit {
    Hit::new(Some(Pt { x, y: 10.0 }), AREA)
}

fn press(x: f32) -> PointerInput {
    mouse(PointerPhase::Down, Some(Pt { x, y: 10.0 }))
}

fn moved(x: f32) -> PointerInput {
    mouse(PointerPhase::Move, Some(Pt { x, y: 10.0 }))
}

/// The press picks the handle it lands nearer to, which is the whole reason
/// one control can write two endpoints.
#[kithara::test]
fn a_press_takes_the_nearer_handle() {
    let span = span();

    let mut low = SpanState::default();
    assert_eq!(
        span.on_input(&mut low, Input::Pointer(press(50.0)), &hit(50.0))
            .value()
            .map(|(edge, _)| edge),
        Some(Edge::Min)
    );

    let mut high = SpanState::default();
    assert_eq!(
        span.on_input(&mut high, Input::Pointer(press(190.0)), &hit(190.0))
            .value()
            .map(|(edge, _)| edge),
        Some(Edge::Max)
    );
}

/// A drag that crosses the other handle keeps writing the endpoint it
/// started on; swapping mid-gesture would fold the interval through itself.
#[kithara::test]
fn a_held_handle_survives_crossing_the_other_one() {
    let span = span();
    let mut state = SpanState::default();
    span.on_input(&mut state, Input::Pointer(press(50.0)), &hit(50.0));

    let outcome = span.on_input(&mut state, Input::Pointer(moved(200.0)), &hit(200.0));

    assert_eq!(outcome.value(), Some((Edge::Min, 0.95)));
}

/// Release gives the pointer back, so the next press picks a handle again.
#[kithara::test]
fn release_ends_the_gesture() {
    let span = span();
    let mut state = SpanState::default();
    span.on_input(&mut state, Input::Pointer(press(190.0)), &hit(190.0));
    assert!(state.captures_pointer());

    span.on_input(
        &mut state,
        Input::Pointer(mouse(PointerPhase::Up, Some(Pt { x: 190.0, y: 10.0 }))),
        &hit(190.0),
    );

    assert!(!state.captures_pointer());
    assert_eq!(
        span.on_input(&mut state, Input::Pointer(moved(50.0)), &hit(50.0))
            .value(),
        None,
        "a move after release belongs to nobody"
    );
}

/// A control laid out to nothing has no position under the pointer at all —
/// `Rect::contains` is half-open, so a zero-width box holds no point. The
/// press is therefore not this control's to take, and leaving it uncaptured
/// is what lets whatever is behind it answer.
#[kithara::test]
fn a_degenerate_box_takes_no_press() {
    let span = span();
    let mut state = SpanState::default();
    let flat = Hit::new(Some(Pt { x: 10.0, y: 10.0 }), Rect { w: 0.0, ..AREA });

    let outcome = span.on_input(&mut state, Input::Pointer(press(10.0)), &flat);

    assert_eq!(outcome.value(), None);
    assert!(!outcome.is_captured());
    assert!(!state.captures_pointer());
}
