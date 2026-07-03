#![cfg(not(target_arch = "wasm32"))]
use std::fmt;

use kithara::{
    events::{AudioEvent, Event, EventReceiver},
    platform::time::{Duration, Instant},
};
use kithara_integration_tests::{flash_pace::virtual_pace, offline::OfflinePlayer};
pub(crate) const CONTINUITY_BLOCK_FRAMES: usize = 512;
pub(crate) const CONTINUITY_SAMPLE_RATE: u32 = 44_100;
const ACTIVE_SAMPLE_THRESHOLD: f32 = 0.001;

#[derive(Debug, Clone)]
pub(crate) struct OutputGapStats {
    pub(crate) label: String,
    pub(crate) blocks: u32,
    pub(crate) max_silence_run: u32,
    pub(crate) max_render: Duration,
    pub(crate) slow_renders: u32,
    block_frames: usize,
    sample_rate: u32,
}

impl OutputGapStats {
    #[must_use]
    pub(crate) fn block_duration_for(block_frames: usize, sample_rate: u32) -> Duration {
        Duration::from_secs_f64(block_frames as f64 / f64::from(sample_rate))
    }

    #[must_use]
    pub(crate) fn block_budget(&self) -> Duration {
        Self::block_duration_for(self.block_frames, self.sample_rate)
    }
}

impl fmt::Display for OutputGapStats {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let silence_ms =
            f64::from(self.max_silence_run) * self.block_budget().as_secs_f64() * 1000.0;
        write!(
            f,
            "{}: {} blocks, silence={} ({:.1}ms) max_render={:?} slow={}",
            self.label,
            self.blocks,
            self.max_silence_run,
            silence_ms,
            self.max_render,
            self.slow_renders,
        )
    }
}

#[derive(Debug, Default)]
pub(crate) struct PlaybackProgressProbe {
    pub(crate) progress_events: usize,
    pub(crate) regressions: usize,
    pub(crate) max_gap_between_events: Duration,
    last_position_ms: Option<u64>,
    last_event_at: Option<Instant>,
}

impl PlaybackProgressProbe {
    pub(crate) fn drain(&mut self, rx: &mut EventReceiver) {
        while let Ok(event) = rx.try_recv() {
            if let Event::Audio(AudioEvent::PlaybackProgress { position_ms, .. }) = event {
                let now = Instant::now();
                if let Some(last) = self.last_event_at {
                    let gap = now.duration_since(last);
                    if gap > self.max_gap_between_events {
                        self.max_gap_between_events = gap;
                    }
                }
                if let Some(prev) = self.last_position_ms
                    && position_ms < prev
                {
                    self.regressions += 1;
                }
                self.last_position_ms = Some(position_ms);
                self.last_event_at = Some(now);
                self.progress_events += 1;
            }
        }
    }

    pub(crate) fn observe_idle(&mut self) {
        if let Some(last) = self.last_event_at {
            let gap = last.elapsed();
            if gap > self.max_gap_between_events {
                self.max_gap_between_events = gap;
            }
        }
    }
}

#[must_use]
pub(crate) fn render_offline_window(
    player: &mut OfflinePlayer,
    blocks: u32,
    label: &str,
    block_frames: usize,
    sample_rate: u32,
) -> OutputGapStats {
    let block_budget = OutputGapStats::block_duration_for(block_frames, sample_rate);
    let mut max_silence = 0u32;
    let mut current_silence = 0u32;
    let mut max_render = Duration::ZERO;
    let mut slow = 0u32;

    for _ in 0..blocks {
        // Render timing stays on REAL time: `slow_renders`/`max_render` measure
        // the actual CPU cost of pulling one block through the graph, which is a
        // wall-clock contract (RTSan / block-budget). `Instant::now`/`elapsed`
        // here read real time because this helper runs with `active=false`.
        let started = Instant::now();
        let out = player.render(block_frames);
        let elapsed = started.elapsed();
        if elapsed > max_render {
            max_render = elapsed;
        }
        if elapsed > block_budget {
            slow += 1;
        }
        if out
            .iter()
            .any(|sample| sample.abs() > ACTIVE_SAMPLE_THRESHOLD)
        {
            if current_silence > max_silence {
                max_silence = current_silence;
            }
            current_silence = 0;
        } else {
            current_silence += 1;
        }
        // Inter-block pacing MUST drive the virtual clock so the decode worker
        // (a `spawn_named` flash pacer parked on the engine) advances and fills
        // the producer ring before the next `render` samples it. `virtual_pace`
        // is the `#[kithara::flash]`-guarded sleep: inside the test driver's poll
        // it is a BRIDGED wait that releases the task's `active_async` slot, lets
        // the clock jump, and re-acquires on resume — so the worker delivers real
        // PCM exactly as on the real clock instead of the render zero-filling
        // silence on underrun. Off the flash feature / off ambient it is a real
        // wall-clock sleep.
        virtual_pace(block_budget.saturating_sub(elapsed));
    }

    if current_silence > max_silence {
        max_silence = current_silence;
    }

    OutputGapStats {
        label: label.to_owned(),
        blocks,
        max_silence_run: max_silence,
        max_render,
        slow_renders: slow,
        block_frames,
        sample_rate,
    }
}
