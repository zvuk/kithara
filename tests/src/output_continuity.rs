//! Output-continuity oracle for offline renders: the longest silent run a
//! render window carried, and whether `PlaybackProgress` ever ran backwards.

use std::fmt;

use kithara::{
    audio::AudioEvent,
    events::EventReceiver,
    platform::time::{Duration, Instant},
};
use kithara_test_utils::virtual_pace;

use crate::{event::TestEvent, offline::OfflinePlayer};

pub const CONTINUITY_BLOCK_FRAMES: usize = 512;
pub const CONTINUITY_SAMPLE_RATE: u32 = 44_100;
const ACTIVE_SAMPLE_THRESHOLD: f32 = 0.001;
/// Blocks a caller may render while waiting for first audible output. At
/// [`CONTINUITY_BLOCK_FRAMES`] and [`CONTINUITY_SAMPLE_RATE`] this is ~5 s of
/// output: past any decoder warm-up, and inside every caller's test budget.
const AUDIBLE_WARMUP_BLOCKS: u32 = 430;

/// One offline render window, judged by what the mix carried.
///
/// `max_render` is wall time and belongs to the log, never to an assertion. The
/// render's own work is a few microseconds of thread CPU; the rest of the call
/// is the round-trip to the host's owner thread, so a budget on it reads how
/// promptly the machine schedules two threads, not how much the graph costs.
/// What the render must not do — wait on a source with nothing ready instead of
/// underrunning it — is pinned on the audio thread, where such a wait is a hang
/// rather than a slow block.
#[derive(Debug, Clone)]
pub struct OutputGapStats {
    pub label: String,
    pub blocks: u32,
    pub max_silence_run: u32,
    pub max_render: Duration,
    block_frames: usize,
    sample_rate: u32,
}

impl OutputGapStats {
    #[must_use]
    pub fn block_duration_for(block_frames: usize, sample_rate: u32) -> Duration {
        Duration::from_secs_f64(block_frames as f64 / f64::from(sample_rate))
    }

    #[must_use]
    pub fn block_budget(&self) -> Duration {
        Self::block_duration_for(self.block_frames, self.sample_rate)
    }
}

impl fmt::Display for OutputGapStats {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let silence_ms =
            f64::from(self.max_silence_run) * self.block_budget().as_secs_f64() * 1000.0;
        write!(
            f,
            "{}: {} blocks, silence={} ({:.1}ms) max_render={:?}",
            self.label, self.blocks, self.max_silence_run, silence_ms, self.max_render,
        )
    }
}

#[derive(Debug, Default)]
pub struct PlaybackProgressProbe {
    pub progress_events: usize,
    pub regressions: usize,
    pub max_gap_between_events: Duration,
    last_position_ms: Option<u64>,
    last_event_at: Option<Instant>,
}

impl PlaybackProgressProbe {
    pub fn drain(&mut self, rx: &mut EventReceiver<TestEvent>) {
        while let Ok(event) = rx.try_recv().map(|env| env.event) {
            if let TestEvent::Audio(AudioEvent::PlaybackProgress { position_ms, .. }) = event {
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

    pub fn observe_idle(&mut self) {
        if let Some(last) = self.last_event_at {
            let gap = last.elapsed();
            if gap > self.max_gap_between_events {
                self.max_gap_between_events = gap;
            }
        }
    }
}

/// Render until the mix carries its first audible block.
///
/// A fixed warm-up window is a start-up budget in disguise. The render loop
/// drives the virtual clock while a segment fetch runs on the real one, so a
/// window counted in blocks can elapse before the first segment has landed;
/// the measurement window that follows then opens on zero-fill and the silence
/// oracle reports a dropout that never happened. Waiting for audible output
/// leaves that oracle measuring steady state alone, while a stream that stays
/// silent still fails, now naming the budget it exhausted.
///
/// # Panics
///
/// Panics when no block is audible within [`AUDIBLE_WARMUP_BLOCKS`].
pub async fn render_until_audible(
    player: &mut OfflinePlayer,
    label: &str,
    block_frames: usize,
    sample_rate: u32,
) {
    let block_budget = OutputGapStats::block_duration_for(block_frames, sample_rate);
    for _ in 0..AUDIBLE_WARMUP_BLOCKS {
        let started = Instant::now();
        let out = player.render(block_frames).await;
        let elapsed = started.elapsed();
        if out
            .iter()
            .any(|sample| sample.abs() > ACTIVE_SAMPLE_THRESHOLD)
        {
            return;
        }
        virtual_pace(block_budget.saturating_sub(elapsed));
    }
    panic!("{label}: no audible block within {AUDIBLE_WARMUP_BLOCKS} rendered blocks");
}

#[must_use]
pub async fn render_offline_window(
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

    for _ in 0..blocks {
        let started = Instant::now();
        let out = player.render(block_frames).await;
        let elapsed = started.elapsed();
        if elapsed > max_render {
            max_render = elapsed;
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
        block_frames,
        sample_rate,
    }
}
