use kithara_platform::time::{Duration, WallInstant};
use kithara_test_utils::kithara;
use masonry::vello::wgpu::SurfaceError;
use winit::dpi::PhysicalPosition;

use super::runner::{Clicks, FrameClock, IdleClock, SurfaceRecovery, surface_recovery};

/// The document is told how long it has really been, so its own time runs
/// at the rate the world does rather than the rate the window is woken at.
#[kithara::test]
fn the_frame_clock_reports_the_gap_between_two_wakes() {
    let start = WallInstant::now();
    let mut frames = FrameClock::new(start);

    assert_eq!(
        frames.step(start + Duration::from_millis(16)),
        Duration::from_millis(16)
    );
    assert_eq!(
        frames.step(start + Duration::from_millis(516)),
        Duration::from_millis(500)
    );
}

#[kithara::test]
fn idle_clock_wakes_once_per_period() {
    let start = WallInstant::now();
    let mut clock = IdleClock::new(start);

    assert!(!clock.wake(start));
    assert!(!clock.wake(start + IdleClock::PERIOD / 2));
    assert!(clock.wake(start + IdleClock::PERIOD));
    assert!(!clock.wake(start + IdleClock::PERIOD));
    assert!(clock.wake(start + IdleClock::PERIOD * 2));
}

/// A double click is two presses close in time and place. Nothing below the
/// window sees presses, so if this does not count them no control can.
#[kithara::test]
fn two_quick_presses_in_the_same_place_are_one_double_click() {
    let mut clicks = Clicks::default();
    let start = Instant::now();
    let at = PhysicalPosition::new(100.0, 100.0);

    assert_eq!(clicks.press(at, start), 1);
    assert_eq!(clicks.press(at, start + Duration::from_millis(120)), 2);
    assert_eq!(clicks.press(at, start + Duration::from_millis(240)), 3);
}

/// A run ends when the hand pauses or moves away, otherwise every later
/// press in a session would read as a deeper multi-click.
#[kithara::test]
fn a_pause_or_a_move_starts_the_count_again() {
    let mut clicks = Clicks::default();
    let start = Instant::now();
    let at = PhysicalPosition::new(100.0, 100.0);

    assert_eq!(clicks.press(at, start), 1);
    assert_eq!(
        clicks.press(at, start + Duration::from_millis(900)),
        1,
        "a press long after the last one starts a new run"
    );
    assert_eq!(
        clicks.press(
            PhysicalPosition::new(140.0, 100.0),
            start + Duration::from_millis(950)
        ),
        1,
        "a press away from the last one starts a new run"
    );
}

#[kithara::test]
fn retries_only_recoverable_surface_errors() {
    assert_eq!(
        surface_recovery(&SurfaceError::Timeout),
        SurfaceRecovery::Retry
    );
    assert_eq!(
        surface_recovery(&SurfaceError::Outdated),
        SurfaceRecovery::Reconfigure
    );
    assert_eq!(
        surface_recovery(&SurfaceError::Lost),
        SurfaceRecovery::Reconfigure
    );
    assert_eq!(
        surface_recovery(&SurfaceError::OutOfMemory),
        SurfaceRecovery::Stop
    );
    assert_eq!(
        surface_recovery(&SurfaceError::Other),
        SurfaceRecovery::Stop
    );
}
