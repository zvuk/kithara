#![forbid(unsafe_code)]

//! Configured lookahead on the shared peak limiter, driven through its public owner API.
//!
//! Fixtures are synthetic stereo impulses at 48 kHz, so every expected frame follows from
//! the configuration: a 5 ms lookahead is 240 frames. Thresholds are exact: delayed audio
//! below the ceiling keeps its bits, limited audio never exceeds the ceiling, and silence
//! stays exact silence.
//!
//! Axes: uneven, equal and whole caller blocks; 1 ms and 50 ms release; left and right
//! planes at a fixed ratio for the linked gain. Coverage: an impulse below the ceiling
//! leaves the delay once, 240 frames later; a transient over the ceiling followed by
//! silent blocks leaves once, within the ceiling, for whole and blocked calls; a
//! transient on the last frame of content drains once at the ceiling.

use std::num::{NonZeroU32, NonZeroUsize};

use kithara_effects::{Ceiling, LimiterConfig, LookaheadMs, PeakLimiter, ReleaseMs};
use kithara_fixture_media::reconstruction::reconstructed_peak;
use kithara_test_utils::{bufpool::pools, kithara};

const RATE: u32 = 48_000;
const CEILING: f32 = 0.98;
const LOOKAHEAD_MS: f32 = 5.0;
const LOOKAHEAD_FRAMES: usize = 240;

fn config(release_ms: f32) -> LimiterConfig {
    LimiterConfig::builder()
        .ceiling(Ceiling::try_from(CEILING).expect("limiter ceiling"))
        .release_ms(ReleaseMs::try_from(release_ms).expect("limiter release"))
        .lookahead_ms(LookaheadMs::try_from(LOOKAHEAD_MS).expect("limiter lookahead"))
        .build()
}

fn limiter(config: LimiterConfig) -> PeakLimiter {
    let rate = NonZeroU32::new(RATE).expect("rate is non-zero");
    PeakLimiter::new(
        rate,
        rate,
        NonZeroUsize::new(2).expect("stereo is non-zero"),
        config,
        &pools(),
    )
    .expect("a stereo limiter fits the detector and the test region")
}

/// Process both planes in place, one caller block at a time.
fn process_blocks(
    limiter: &mut PeakLimiter,
    left: &mut [f32],
    right: &mut [f32],
    blocks: &[usize],
) {
    let mut start = 0;
    for &block in blocks {
        let end = start + block;
        limiter.process_planar(&mut [&mut left[start..end], &mut right[start..end]]);
        start = end;
    }
    assert_eq!(start, left.len(), "blocks cover the planes");
}

fn bits(values: &[f32]) -> Vec<u32> {
    values.iter().copied().map(f32::to_bits).collect()
}

#[kithara::test]
fn nonzero_lookahead_delays_an_impulse_by_the_reported_frames() {
    let config = config(50.0);
    let rate = NonZeroU32::new(RATE).expect("rate is non-zero");
    assert_eq!(config.latency_frames(rate), LOOKAHEAD_FRAMES);
    let mut limiter = limiter(config);
    let frames = 10 + LOOKAHEAD_FRAMES + 50;
    let mut left = vec![0.0_f32; frames];
    let mut right = vec![0.0_f32; frames];
    left[10] = 0.5;
    right[10] = -0.25;
    process_blocks(&mut limiter, &mut left, &mut right, &[7, 64, 1, 128, 100]);
    let mut expected_left = vec![0.0_f32; frames];
    let mut expected_right = vec![0.0_f32; frames];
    expected_left[10 + LOOKAHEAD_FRAMES] = 0.5;
    expected_right[10 + LOOKAHEAD_FRAMES] = -0.25;
    assert_eq!(bits(&left), bits(&expected_left));
    assert_eq!(bits(&right), bits(&expected_right));
}

#[kithara::test]
fn silent_blocks_after_a_transient_keep_the_delayed_audio_once() {
    const BLOCK: usize = 64;
    let frames = BLOCK + LOOKAHEAD_FRAMES.next_multiple_of(BLOCK) + BLOCK;
    let input = |frame: usize| {
        if frame == BLOCK - 2 || frame == BLOCK - 1 {
            1.96
        } else {
            0.0
        }
    };
    let mut left: Vec<f32> = (0..frames).map(input).collect();
    let mut right: Vec<f32> = left.iter().map(|sample| sample * 0.5).collect();
    let mut whole = limiter(config(50.0));
    let (mut whole_left, mut whole_right) = (left.clone(), right.clone());
    process_blocks(&mut whole, &mut whole_left, &mut whole_right, &[frames]);
    let mut blocked = limiter(config(50.0));
    let blocks = vec![BLOCK; frames / BLOCK];
    process_blocks(&mut blocked, &mut left, &mut right, &blocks);
    // External FIR blocks hold their last sample; the gain can differ by partition.
    for (partition, left, right) in [
        ("whole", &whole_left, &whole_right),
        ("blocked", &left, &right),
    ] {
        let sounding: Vec<usize> = (0..frames).filter(|&frame| left[frame] != 0.0).collect();
        assert_eq!(
            sounding,
            [BLOCK - 2 + LOOKAHEAD_FRAMES, BLOCK - 1 + LOOKAHEAD_FRAMES],
            "{partition}: the transient leaves the delay once, at the reported frames"
        );
        for frame in sounding {
            assert!(
                left[frame] <= CEILING,
                "{partition}, frame {frame}: {}",
                left[frame]
            );
            assert_eq!(
                right[frame].to_bits(),
                (left[frame] * 0.5).to_bits(),
                "{partition}: linked gain"
            );
        }
    }
}

#[kithara::test]
fn a_final_delayed_transient_holds_the_ceiling_through_the_drain() {
    const CONTENT: usize = 1_000;
    for release_ms in [1.0, 50.0] {
        let mut limiter = limiter(config(release_ms));
        let frames = CONTENT + LOOKAHEAD_FRAMES;
        let mut left = vec![0.0_f32; frames];
        left[CONTENT - 1] = 1.96;
        let mut right = left.clone();
        process_blocks(
            &mut limiter,
            &mut left,
            &mut right,
            &[CONTENT, LOOKAHEAD_FRAMES],
        );
        let sounding: Vec<usize> = (0..frames).filter(|&frame| left[frame] != 0.0).collect();
        assert_eq!(
            sounding,
            [CONTENT - 1 + LOOKAHEAD_FRAMES],
            "release {release_ms} ms"
        );
        let peak = left[CONTENT - 1 + LOOKAHEAD_FRAMES];
        assert!(
            peak <= CEILING,
            "release {release_ms} ms: the drained transient reaches {peak}, over the ceiling {CEILING}"
        );
        assert_eq!(bits(&left), bits(&right), "linked gain");
    }
}

#[kithara::test]
fn delayed_block_edges_hold_the_independent_true_peak_ceiling() {
    const BLOCK: usize = 128;
    let frames = 8 * BLOCK + LOOKAHEAD_FRAMES.next_multiple_of(BLOCK) + BLOCK;
    for partition in [1, 7, BLOCK, 1024, frames] {
        let mut left: Vec<f32> = (0..frames)
            .map(|frame| {
                if frame < 8 * BLOCK {
                    [4.0, 0.0, 0.0, 0.25][frame / BLOCK % 4]
                } else {
                    0.0
                }
            })
            .collect();
        let mut right: Vec<f32> = left.iter().map(|sample| sample * 0.5).collect();
        let mut limited = limiter(config(50.0));
        for (left, right) in left.chunks_mut(partition).zip(right.chunks_mut(partition)) {
            limited.process_planar(&mut [left, right]);
        }
        assert!(left.iter().any(|sample| *sample != 0.0));
        assert!(left.iter().all(|sample| sample.abs() <= CEILING));
        assert_eq!(
            bits(&right),
            bits(&left.iter().map(|sample| sample * 0.5).collect::<Vec<_>>())
        );
        let over_db = 20.0 * (reconstructed_peak(&left) / CEILING).log10();
        assert!(
            over_db <= 0.1,
            "partition {partition}: delayed edges exceed the ceiling by {over_db} dB"
        );
    }
}
