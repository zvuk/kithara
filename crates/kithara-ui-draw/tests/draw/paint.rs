use kithara_test_utils::kithara;
use kithara_ui_draw::{Paint, Rgba, Stop, Stops, StopsError};

const BLACK: Rgba = Rgba {
    a: 1.0,
    b: 0.0,
    g: 0.0,
    r: 0.0,
};

fn stop(offset: f32) -> Stop {
    Stop {
        offset,
        color: BLACK,
    }
}

/// A ramp a backend cannot interpolate must not be constructible, so no
/// backend has to decide what to do with one.
#[kithara::test]
fn a_ramp_is_checked_before_it_exists() {
    assert_eq!(Stops::new(&[]), Err(StopsError::TooFew { count: 0 }));
    assert_eq!(
        Stops::new(&[stop(0.0)]),
        Err(StopsError::TooFew { count: 1 })
    );
    assert_eq!(
        Stops::new(&[stop(0.0), stop(0.3), stop(0.6), stop(0.8), stop(1.0)]),
        Err(StopsError::TooMany { count: 5 })
    );
    assert_eq!(
        Stops::new(&[stop(0.0), stop(1.5)]),
        Err(StopsError::Offset { index: 1 })
    );
    assert_eq!(
        Stops::new(&[stop(0.0), stop(f32::NAN)]),
        Err(StopsError::Offset { index: 1 })
    );
    assert_eq!(
        Stops::new(&[stop(0.6), stop(0.2)]),
        Err(StopsError::Order { index: 1 })
    );
}

#[kithara::test]
fn a_kept_ramp_reads_back_exactly_what_it_was_given() {
    let given = [stop(0.0), stop(0.4), stop(1.0)];
    let stops = Stops::new(&given).unwrap_or_else(|error| panic!("a valid ramp: {error}"));

    assert_eq!(stops.as_slice(), given);
}

/// A colour is a paint, so a caller that has never heard of gradients keeps
/// passing colours.
#[kithara::test]
fn a_colour_is_a_paint() {
    assert_eq!(Paint::from(BLACK), Paint::Solid(BLACK));
}
