use std::io::Write as _;

use kithara_platform::{
    flash::{self, Instant},
    time::WallInstant,
};

/// Measures the selected clock across a generated native test entry.
///
/// Clock snapshots restore the caller's mode before returning. The measurement
/// holds no mode scope across the test body, cancellation or runtime shutdown.
#[derive(Debug)]
#[must_use]
pub struct ClockMeasurement {
    name: &'static str,
    started: Instant,
    wall_started: WallInstant,
    flash: bool,
}

impl ClockMeasurement {
    /// Snapshot the requested clock when `KITHARA_TEST_CLOCK_MEASUREMENT` is present.
    #[must_use]
    pub fn start(name: &'static str, on: bool) -> Option<Self> {
        std::env::var_os("KITHARA_TEST_CLOCK_MEASUREMENT").map(|_| {
            let wall_started = WallInstant::now();
            let (flash, started) = clock_snapshot(on);
            Self {
                name,
                started,
                wall_started,
                flash,
            }
        })
    }
}

impl Drop for ClockMeasurement {
    fn drop(&mut self) {
        let pid = std::process::id();
        let wall_elapsed_ns = self.wall_started.elapsed().as_nanos();
        let elapsed = clock_snapshot(self.flash)
            .1
            .duration_since(self.started)
            .as_nanos();
        let mode = if self.flash { "flash" } else { "real" };
        let _ = writeln!(
            std::io::stderr().lock(),
            "KITHARA_TEST_CLOCK {{\"test\":\"{}\",\"pid\":{pid},\"mode\":\"{mode}\",\"elapsed_ns\":{elapsed},\"wall_elapsed_ns\":{wall_elapsed_ns}}}",
            self.name,
        );
    }
}

fn clock_snapshot(on: bool) -> (bool, Instant) {
    let _ambient = flash::ambient_scope(on);
    active_clock()
}

#[kithara_test_macros::flash(true)]
fn active_clock() -> (bool, Instant) {
    (flash::ambient_snapshot(), Instant::now())
}
