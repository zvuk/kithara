use super::GAPLESS_SAMPLE_RATE;

pub const BLOCK_FRAMES: usize = 512;
pub const POST_ROLL_BLOCKS: usize = 8;
pub(super) const TRAILING_FRAMES: u64 = 1;

pub struct ContinuityMetric;

impl ContinuityMetric {
    pub const CONTROL_STRIDE_FRAMES: usize = GAPLESS_SAMPLE_RATE as usize / 4;
    pub const HALF_WINDOW_FRAMES: usize = 64;
    pub const MAX_RATIO: f32 = 3.0;
    pub const MIN_CONTROL_WINDOWS: usize = 2;
}
/// How much audio a wait for an item's end may render before it reports that
/// the item never ended.
///
/// Rendering is what moves that wait forward, so rendered audio is what bounds
/// it. A clock cannot: `Instant::now()` under flash reads the virtual clock,
/// which the engine fast-forwards to the next pending deadline whenever every
/// task is parked, and a handover between two items is exactly such a moment.
/// A stress attempt that ran for 659 ms of real time panicked on a deadline of
/// 15 virtual seconds with the dump recording that clock at 86416 s, while the
/// item it waited for had already started and only needed more blocks. The
/// longest material any caller here renders is a few seconds, so this bounds a
/// player that stopped advancing without bounding one that is merely waiting
/// to be scheduled.
pub const ITEM_END_BUDGET_FRAMES: usize = 64 * GAPLESS_SAMPLE_RATE as usize;
pub const SILENCE_THRESHOLD: f32 = 1.0e-3;
pub const FUSED_FIXTURE_SOURCE_RATE: u32 = 44_100;
pub const FUSED_FIXTURE_DEVICE_RATE: u32 = 48_000;
pub const FUSED_FIXTURE_SOURCE_FRAMES: u64 = 44_100;
pub const FUSED_FIXTURE_IDEAL_DEVICE_FRAMES: usize = 48_000;
pub const FUSED_FIXTURE_MASTER_LEVEL: f32 = 0.32;
#[cfg(all(
    feature = "apple-fused-src",
    any(target_os = "macos", target_os = "ios")
))]
pub const APPLE_FUSED_DEFICIT_SOURCE_FRAMES: u64 = 261_120;
#[cfg(all(
    feature = "apple-fused-src",
    any(target_os = "macos", target_os = "ios")
))]
pub const APPLE_FUSED_DEFICIT_SEGMENT_SECS: f64 = 5.921_088_435_374_15;
#[cfg(all(
    feature = "apple-fused-src",
    any(target_os = "macos", target_os = "ios")
))]
pub const APPLE_FUSED_DEFICIT_SEGMENTS: usize = 1;
/// Phase-locked tone: period = 100 frames at 48 kHz, integer divisor of the
/// sample rate, integer divisor of the AAC frame size (1024 / 100 ≠ int but
/// the wave is still perfectly periodic over any block large enough — only
/// the segment-boundary alignment matters here).
pub const SINE_HZ: f64 = 480.0;
