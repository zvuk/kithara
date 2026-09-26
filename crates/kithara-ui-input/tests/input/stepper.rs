use kithara_platform::time::Duration;
use kithara_test_utils::kithara;
use kithara_ui_draw::{Pt, Rect};
use kithara_ui_input::{
    Hit, Input, Outcome, PointerOwnership, PointerPhase, Scroll, mouse as mouse_input,
    recognizers::{StepEvent, Stepper},
};

fn surface() -> Rect {
    Rect {
        h: 38.0,
        w: 80.0,
        x: 0.0,
        y: 0.0,
    }
}

fn at(y: f32) -> Hit {
    Hit::new(Some(Pt { y, x: 40.0 }), surface())
}

fn inside() -> Hit {
    at(19.0)
}

fn moved(y: f32) -> Input<'static> {
    Input::Pointer(mouse_input(PointerPhase::Move, Some(Pt { y, x: 40.0 })))
}

fn pointer(phase: PointerPhase) -> Input<'static> {
    Input::Pointer(mouse_input(phase, None))
}

#[kithara::test]
fn a_detent_over_the_surface_reports_its_direction() {
    let now = Instant::now();

    for (delta, expected) in [(-1.0, 1.0), (1.0, -1.0)] {
        assert_eq!(
            Stepper::default().on_input(Input::Wheel(Scroll::lines(delta)), &inside(), now),
            Outcome::set(StepEvent::By(expected))
        );
    }
    assert_eq!(
        Stepper::default().on_input(Input::Wheel(Scroll::lines(-1.0)), &at(400.0), now),
        Outcome::IGNORED,
        "a detent outside the surface belongs to whatever is under it"
    );
}

#[kithara::test]
fn a_trackpad_gesture_steps_once_and_its_momentum_adds_nothing() {
    let mut stepper = Stepper::default();
    let now = Instant::now();

    assert_eq!(
        stepper.on_input(
            Input::Wheel(Scroll::Pixels { x: 0.0, y: -14.0 }),
            &inside(),
            now
        ),
        Outcome::set(StepEvent::By(1.0)),
        "any pixel delta is one detent, not an accumulated fraction"
    );

    for tail in [-11.0, -8.0, -5.0, -3.0, -1.5, -0.6, -0.2] {
        assert_eq!(
            stepper.on_input(
                Input::Wheel(Scroll::Pixels { x: 0.0, y: tail }),
                &inside(),
                now + Duration::from_millis(199),
            ),
            Outcome::captured(),
            "the momentum tail is owned but must not step the value"
        );
    }
    assert_eq!(
        stepper.on_input(
            Input::Wheel(Scroll::Pixels { x: 0.0, y: -11.0 }),
            &inside(),
            now + Duration::from_millis(200),
        ),
        Outcome::set(StepEvent::By(1.0)),
        "the window is exclusive, so a delta 200 ms on is a new gesture"
    );
}

#[kithara::test]
fn a_double_click_over_the_surface_activates_it() {
    let mut stepper = Stepper::default();
    let now = Instant::now();

    assert_eq!(
        stepper.on_input(pointer(PointerPhase::Down), &inside(), now),
        Outcome::IGNORED.with_ownership(PointerOwnership::Claim),
        "a lone press arms the drag and takes the pointer, leaving the event free"
    );
    assert_eq!(
        stepper.on_input(pointer(PointerPhase::Down), &inside(), now),
        Outcome::set(StepEvent::Activate)
    );
    assert_eq!(
        stepper.on_input(moved(11.0), &at(11.0), now),
        Outcome::IGNORED,
        "the second press of a pair must not drag the value away from the reset"
    );
    assert_eq!(
        stepper.on_input(pointer(PointerPhase::Down), &inside(), now),
        Outcome::IGNORED.with_ownership(PointerOwnership::Claim),
        "the pair is spent, so the next press starts a new one"
    );
}

#[kithara::test]
fn a_held_press_steps_the_value_by_the_travel_it_drags() {
    let mut stepper = Stepper::default();
    let now = Instant::now();

    assert_eq!(
        stepper.on_input(pointer(PointerPhase::Down), &inside(), now),
        Outcome::IGNORED.with_ownership(PointerOwnership::Claim)
    );
    assert_eq!(
        stepper.on_input(moved(11.0), &at(11.0), now),
        Outcome::set(StepEvent::By(2.0)),
        "dragging up must step the value up"
    );
    assert_eq!(
        stepper.on_input(moved(27.0), &at(27.0), now),
        Outcome::set(StepEvent::By(-4.0)),
        "the travel is measured from where the last step left off"
    );

    assert_eq!(
        stepper.on_input(pointer(PointerPhase::Up), &at(27.0), now),
        Outcome::captured().with_ownership(PointerOwnership::Release),
        "release ends the drag and gives the pointer back"
    );
    assert_eq!(
        stepper.on_input(moved(3.0), &at(3.0), now),
        Outcome::IGNORED,
        "travel after the release belongs to nobody"
    );
}

/// A host that expresses the hit in the flow's own space still reports the
/// event in the window's. Travel measured across the two is the distance
/// between the spaces, not the distance the hand moved.
#[kithara::test]
fn the_press_measures_travel_against_the_event_and_not_the_local_hit() {
    let mut stepper = Stepper::default();
    let now = Instant::now();
    let local = |y| Hit::new(Some(Pt { y, x: 40.0 }), surface());

    stepper.on_input(
        Input::Pointer(mouse_input(
            PointerPhase::Down,
            Some(Pt { x: 40.0, y: 50.0 }),
        )),
        &local(10.0),
        now,
    );

    assert_eq!(
        stepper.on_input(moved(42.0), &local(2.0), now),
        Outcome::set(StepEvent::By(2.0)),
    );
}

#[kithara::test]
fn a_cancelled_drag_gives_the_pointer_back_and_leaves_nothing_armed() {
    let mut stepper = Stepper::default();
    let now = Instant::now();

    stepper.on_input(pointer(PointerPhase::Down), &inside(), now);

    assert_eq!(
        stepper.on_input(pointer(PointerPhase::Cancel), &inside(), now),
        Outcome::captured().with_ownership(PointerOwnership::Release)
    );
    assert_eq!(
        stepper.on_input(moved(3.0), &at(3.0), now),
        Outcome::IGNORED,
        "travel after the cancel belongs to nobody"
    );
}

#[kithara::test]
fn the_surface_leaves_every_other_event_to_the_controls_it_wraps() {
    let now = Instant::now();

    for input in [pointer(PointerPhase::Up), moved(11.0)] {
        assert_eq!(
            Stepper::default().on_input(input, &inside(), now),
            Outcome::IGNORED
        );
    }
}
