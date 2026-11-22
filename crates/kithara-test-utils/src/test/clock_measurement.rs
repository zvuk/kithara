#[cfg(not(target_arch = "wasm32"))]
use std::io::Write as _;

#[cfg(target_arch = "wasm32")]
use js_sys::{Function, Reflect, global};
use kithara_platform::{
    flash::{self, Instant},
    time::WallInstant,
};
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::{JsCast as _, JsValue};

/// Measures the selected clock across a generated test entry.
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
    /// Snapshot the clock when `KITHARA_TEST_CLOCK_MEASUREMENT` is present.
    /// Wasm reads the build environment; native reads the invocation environment.
    #[must_use]
    pub fn start(name: &'static str, on: bool) -> Option<Self> {
        #[cfg(not(target_arch = "wasm32"))]
        let enabled = std::env::var_os("KITHARA_TEST_CLOCK_MEASUREMENT").is_some();
        #[cfg(target_arch = "wasm32")]
        let enabled = option_env!("KITHARA_TEST_CLOCK_MEASUREMENT").is_some();
        enabled.then(|| {
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
        #[cfg(not(target_arch = "wasm32"))]
        let pid = std::process::id();
        #[cfg(not(target_arch = "wasm32"))]
        let identity = format_args!("\"pid\":{pid},");
        #[cfg(target_arch = "wasm32")]
        let identity = "";
        let wall_elapsed_ns = self.wall_started.elapsed().as_nanos();
        let elapsed = clock_snapshot(self.flash)
            .1
            .duration_since(self.started)
            .as_nanos();
        let mode = if self.flash { "flash" } else { "real" };
        let line = format_args!(
            "KITHARA_TEST_CLOCK {{\"test\":\"{}\",{identity}\"mode\":\"{mode}\",\"elapsed_ns\":{elapsed},\"wall_elapsed_ns\":{wall_elapsed_ns}}}",
            self.name,
        );
        #[cfg(not(target_arch = "wasm32"))]
        let _ = writeln!(std::io::stderr().lock(), "{line}");
        #[cfg(target_arch = "wasm32")]
        let _ = record_clock(&line.to_string());
    }
}

#[cfg(target_arch = "wasm32")]
fn record_clock(line: &str) -> Result<(), JsValue> {
    let console = Reflect::get(&global(), &JsValue::from_str("console"))?;
    let log = Reflect::get(&console, &JsValue::from_str("log"))?.dyn_into::<Function>()?;
    log.call1(&console, &JsValue::from_str(line)).map(|_| ())
}

fn clock_snapshot(on: bool) -> (bool, Instant) {
    let _ambient = flash::ambient_scope(on);
    active_clock()
}

#[kithara_test_macros::flash(true)]
fn active_clock() -> (bool, Instant) {
    (flash::ambient_snapshot(), Instant::now())
}

#[cfg(all(test, target_arch = "wasm32"))]
mod tests {
    use kithara_platform::time::{Duration, sleep};
    use wasm_bindgen::{JsValue, closure::Closure, prelude::wasm_bindgen};

    use super::{ClockMeasurement, WallInstant, clock_snapshot};
    use crate::kithara;

    #[wasm_bindgen(inline_js = r#"
export function captureClockReport(run) {
    const original = console.log;
    let report;
    console.log = (line, ...args) => {
        if (typeof line === "string" && line.startsWith("KITHARA_TEST_CLOCK ")) report = line;
        else original.call(console, line, ...args);
    };
    try { run(); return report; }
    finally { console.log = original; }
}
"#)]
    extern "C" {
        #[wasm_bindgen(catch, js_name = captureClockReport)]
        fn capture_clock_report(run: &JsValue) -> Result<Option<String>, JsValue>;
    }

    #[kithara::test(wasm, flash(false))]
    async fn a_browser_clock_report_includes_the_body_wait_without_a_process_id() {
        let wall_started = WallInstant::now();
        let (flash, started) = clock_snapshot(true);
        assert!(!flash, "the browser has no virtual clock backend");
        let measurement = ClockMeasurement {
            name: "browser-clock-report",
            started,
            wall_started,
            flash,
        };
        let wait_started = WallInstant::now();
        sleep(Duration::from_millis(5)).await;
        let minimum = wait_started.elapsed().as_nanos();
        let run = Closure::once(move || drop(measurement));
        let line = capture_clock_report(run.as_ref())
            .expect("capture browser clock receipt")
            .expect("browser clock receipt");
        let report: serde_json::Value = serde_json::from_str(
            line.strip_prefix("KITHARA_TEST_CLOCK ")
                .expect("clock report prefix"),
        )
        .expect("clock report JSON");
        assert_eq!(report["test"], "browser-clock-report");
        assert_eq!(report["mode"], "real");
        assert!(report.get("pid").is_none());
        for field in ["elapsed_ns", "wall_elapsed_ns"] {
            let elapsed = report[field].as_u64().expect("elapsed nanoseconds");
            assert!(u128::from(elapsed) >= minimum, "{report}");
        }
    }

    #[kithara::test(wasm, flash(false))]
    fn browser_clock_measurement_follows_the_build_setting() {
        let measurement = ClockMeasurement::start("browser-opt-in", true);
        let run = Closure::once(move || drop(measurement));
        let line = capture_clock_report(run.as_ref()).expect("capture optional clock receipt");
        assert_eq!(
            line.is_some(),
            option_env!("KITHARA_TEST_CLOCK_MEASUREMENT").is_some(),
            "the browser follows the build's opt-in"
        );
        if let Some(line) = line {
            let report: serde_json::Value = serde_json::from_str(
                line.strip_prefix("KITHARA_TEST_CLOCK ")
                    .expect("clock report prefix"),
            )
            .expect("clock report JSON");
            assert_eq!(report["test"], "browser-opt-in");
            assert_eq!(report["mode"], "real");
            assert!(report["wall_elapsed_ns"].as_u64().is_some());
        }
    }
}
