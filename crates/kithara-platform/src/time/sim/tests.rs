use std::sync::Mutex;

use super::{Duration, Instant, advance, reset};

/// Serialize cases that share the process-global `SIM_NANOS`. nextest gives each
/// test its own process, but a thread-based runner would race without this.
static GUARD: Mutex<()> = Mutex::new(());

#[test]
fn now_advances_only_on_advance() {
    let _g = GUARD.lock().unwrap();
    reset();
    let t0 = Instant::now();
    assert_eq!(Instant::now(), t0, "clock frozen between advances");
    advance(Duration::from_millis(10));
    let t1 = Instant::now();
    assert_eq!(t1.duration_since(t0), Duration::from_millis(10));
    assert!(t1 > t0);
}

#[test]
fn elapsed_tracks_virtual_clock() {
    let _g = GUARD.lock().unwrap();
    reset();
    let start = Instant::now();
    assert_eq!(start.elapsed(), Duration::ZERO);
    advance(Duration::from_secs(5));
    assert_eq!(start.elapsed(), Duration::from_secs(5));
}

#[test]
fn arithmetic_saturates_and_orders() {
    let _g = GUARD.lock().unwrap();
    reset();
    let now = Instant::now();
    let later = now + Duration::from_secs(1);
    let earlier = now - Duration::from_secs(1);
    assert!(earlier < now && now < later);
    assert_eq!(later - now, Duration::from_secs(1));
    assert_eq!(later.duration_since(now), Duration::from_secs(1));
    assert_eq!(now.saturating_duration_since(later), Duration::ZERO);
}

#[test]
fn base_keeps_backward_offset_positive() {
    let _g = GUARD.lock().unwrap();
    reset();
    let now = Instant::now();
    let earlier = now - Duration::from_secs(3600);
    assert_eq!(now.duration_since(earlier), Duration::from_secs(3600));
}
