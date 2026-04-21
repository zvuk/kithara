//! Seek-hang watchdog.
//!
//! Observes `Queue::position_seconds` after a seek. If the player is
//! still playing but the reported position doesn't advance within the
//! configured timeout, fires a state dump (to `tracing::error!` and a
//! JSON file in the system temp directory) and panics via
//! [`HangDetector`]. Converts silent deadlocks in the HLS / decoder
//! pipeline into loud, post-mortem-ready failures.

use std::{
    fs::File,
    io::Write,
    path::PathBuf,
    sync::{Arc, Mutex, PoisonError},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use kithara_events::TrackId;
use kithara_hang_detector::HangDetector;
use tracing::error;

/// How long the watchdog waits for position to advance after a seek
/// before declaring a hang. Tuned to be longer than typical HLS segment
/// fetch latency (2-3 s) yet short enough to surface a real freeze
/// before the user gives up on the app.
pub(crate) const SEEK_HANG_TIMEOUT: Duration = Duration::from_secs(5);

/// Minimum position delta that counts as "making progress". Below this
/// threshold we treat the position as stuck.
const PROGRESS_EPSILON_SECS: f64 = 0.1;

/// Frozen snapshot of queue state captured when a seek hang is detected.
/// Written to a JSON file and logged verbatim so post-mortem can tie
/// the hang to the specific track / target / moment.
#[derive(Debug)]
pub(crate) struct SeekHangContext {
    pub track_id: Option<TrackId>,
    pub track_name: Option<String>,
    pub track_url: Option<String>,
    pub seek_target_seconds: f64,
    pub seek_wallclock_ms: u128,
    pub last_observed_position: Option<f64>,
    pub duration_seconds: Option<f64>,
    pub is_playing: bool,
    pub rate: f32,
}

impl SeekHangContext {
    fn to_json(&self, detected_at_ms: u128) -> String {
        let track_id = self
            .track_id
            .map_or_else(|| "null".to_string(), |id| id.as_u64().to_string());
        let name = self
            .track_name
            .as_deref()
            .map_or_else(|| "null".to_string(), json_string);
        let url = self
            .track_url
            .as_deref()
            .map_or_else(|| "null".to_string(), json_string);
        let last_pos = self
            .last_observed_position
            .map_or_else(|| "null".to_string(), |v| format!("{v:.3}"));
        let duration = self
            .duration_seconds
            .map_or_else(|| "null".to_string(), |v| format!("{v:.3}"));
        format!(
            r#"{{"track_id":{track_id},"track_name":{name},"track_url":{url},"seek_target_seconds":{:.3},"seek_wallclock_ms":{},"last_observed_position":{},"duration_seconds":{},"is_playing":{},"rate":{:.3},"detected_at_ms":{}}}"#,
            self.seek_target_seconds,
            self.seek_wallclock_ms,
            last_pos,
            duration,
            self.is_playing,
            self.rate,
            detected_at_ms,
        )
    }
}

fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// Active watchdog for one in-flight seek. Created on `Queue::seek`,
/// polled from `Queue::tick`, cleared once progress is observed (or
/// panics on hang through the embedded [`HangDetector`]).
pub(crate) struct SeekWatchdog {
    detector: HangDetector,
    /// Context snapshot captured at seek issue time, shared with the
    /// `on_hang` callback so the dump written right before panic has
    /// the same view the user caused.
    ctx: Arc<Mutex<SeekHangContext>>,
    baseline_position: Option<f64>,
}

impl SeekWatchdog {
    pub(crate) fn new(ctx: SeekHangContext) -> Self {
        let ctx = Arc::new(Mutex::new(ctx));
        let ctx_for_hook = Arc::clone(&ctx);
        let detector =
            HangDetector::new("queue.seek_progress", SEEK_HANG_TIMEOUT).with_on_hang(move || {
                let snapshot = ctx_for_hook.lock().unwrap_or_else(PoisonError::into_inner);
                dump_hang(&snapshot);
            });
        Self {
            detector,
            ctx,
            baseline_position: None,
        }
    }

    /// Feed one observation from `Queue::tick`. Returns `true` when the
    /// watchdog has confirmed progress and should be dropped by the
    /// caller; `false` while it should keep watching. Panics internally
    /// through [`HangDetector::tick`] if no progress is seen before the
    /// timeout.
    pub(crate) fn observe(&mut self, is_playing: bool, position: Option<f64>, rate: f32) -> bool {
        {
            let mut ctx = self.ctx.lock().unwrap_or_else(PoisonError::into_inner);
            ctx.is_playing = is_playing;
            ctx.rate = rate;
            ctx.last_observed_position = position.or(ctx.last_observed_position);
        }

        if !is_playing {
            // Paused — user intent, not a hang. Keep baseline so that
            // when playback resumes we still measure progress from the
            // last known post-seek position.
            self.detector.reset();
            return false;
        }

        match (self.baseline_position, position) {
            (None, Some(cur)) => {
                self.baseline_position = Some(cur);
                self.detector.reset();
                false
            }
            (Some(prev), Some(cur)) if (cur - prev).abs() > PROGRESS_EPSILON_SECS => {
                // Position moved — seek confirmed live.
                true
            }
            _ => {
                // No progress yet. HangDetector panics after timeout.
                self.detector.tick();
                false
            }
        }
    }
}

fn dump_hang(ctx: &SeekHangContext) {
    let detected_at = now_ms();
    let json = ctx.to_json(detected_at);

    error!(
        target: "kithara_queue::seek_watchdog",
        track_id = ?ctx.track_id,
        track_name = ?ctx.track_name,
        track_url = ?ctx.track_url,
        seek_target_seconds = ctx.seek_target_seconds,
        last_observed_position = ?ctx.last_observed_position,
        duration_seconds = ?ctx.duration_seconds,
        is_playing = ctx.is_playing,
        rate = ctx.rate,
        detected_at_ms = detected_at,
        "seek watchdog: no progress after seek — player appears hanged"
    );

    let path: PathBuf = std::env::temp_dir().join(format!("kithara-seek-hang-{detected_at}.json"));
    match File::create(&path).and_then(|mut f| f.write_all(json.as_bytes())) {
        Ok(()) => {
            error!(target: "kithara_queue::seek_watchdog", path = %path.display(), "wrote hang dump");
        }
        Err(err) => {
            error!(target: "kithara_queue::seek_watchdog", error = %err, path = %path.display(), "failed to write hang dump");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        panic::{AssertUnwindSafe, catch_unwind},
        sync::atomic::{AtomicBool, Ordering},
        thread::sleep,
    };

    use kithara_test_utils::kithara;

    use super::*;

    fn ctx() -> SeekHangContext {
        SeekHangContext {
            track_id: Some(TrackId(0)),
            track_name: Some("test".into()),
            track_url: Some("https://example.com/a.m3u8".into()),
            seek_target_seconds: 42.0,
            seek_wallclock_ms: now_ms(),
            last_observed_position: None,
            duration_seconds: Some(120.0),
            is_playing: true,
            rate: 1.0,
        }
    }

    #[kithara::test]
    fn observe_clears_watchdog_when_position_advances() {
        let mut wd = SeekWatchdog::new(ctx());
        assert!(!wd.observe(true, Some(42.0), 1.0));
        assert!(wd.observe(true, Some(43.0), 1.0));
    }

    #[kithara::test]
    fn observe_paused_does_not_tick_detector() {
        let mut wd = SeekWatchdog::new(ctx());
        // Even with no advance, being paused resets the timer and does
        // not count against the hang budget.
        assert!(!wd.observe(false, Some(42.0), 0.0));
        assert!(!wd.observe(false, Some(42.0), 0.0));
    }

    #[kithara::test(native)]
    fn observe_stalled_playback_panics_via_hang_detector() {
        // Build a watchdog with the default (5s) timeout, then replace
        // its detector with a millisecond-scale one so the test stays
        // fast. We use `catch_unwind` to inspect the panic message and
        // a side-effect flag set by the dump hook.
        let hook_fired = Arc::new(AtomicBool::new(false));
        let hook_fired_clone = Arc::clone(&hook_fired);
        let result = catch_unwind(AssertUnwindSafe(|| {
            let ctx_arc = Arc::new(Mutex::new(ctx()));
            let detector = HangDetector::new("queue.seek_progress.test", Duration::from_millis(5))
                .with_on_hang(move || {
                    hook_fired_clone.store(true, Ordering::SeqCst);
                });
            let mut wd = SeekWatchdog {
                detector,
                ctx: ctx_arc,
                baseline_position: Some(42.0),
            };
            // First observe: no advance, HangDetector ticks and is within deadline.
            let _ = wd.observe(true, Some(42.0), 1.0);
            sleep(Duration::from_millis(20));
            // Second observe: still no advance, past deadline — panic.
            let _ = wd.observe(true, Some(42.0), 1.0);
        }));
        assert!(result.is_err(), "stalled playback must panic");
        assert!(
            hook_fired.load(Ordering::SeqCst),
            "on_hang dump hook must fire before panic"
        );
    }
}
