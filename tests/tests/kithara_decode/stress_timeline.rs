//! Stress test verifying timeline integrity under rapid seeks.
//!
//! Generates a 10-second WAV, creates a decoder, then performs 200 random
//! seek+decode cycles. After each seek, verifies that `PcmMeta` fields are
//! self-consistent: `frame_offset` approximates the seek target,
//! `timestamp` matches `frame_offset / sample_rate`, and monotonicity holds
//! for consecutive chunks within a read burst.

use std::{io::Cursor, time::Duration};

use kithara::decode::{DecoderConfig, DecoderFactory};
use kithara_test_utils::{Xorshift64, wav::create_test_wav};
use rstest::rstest;

const SAMPLE_RATE: u32 = 44100;
const DURATION_SECS: f64 = 10.0;
const SAMPLE_COUNT: usize = (SAMPLE_RATE as f64 * DURATION_SECS) as usize;
const SEEK_ITERATIONS: usize = 200;
const CHUNKS_PER_BURST: usize = 5;

#[rstest]
#[timeout(Duration::from_secs(30))]
#[test]
fn stress_seeks_preserve_timeline_integrity() {
    let wav_data = create_test_wav(SAMPLE_COUNT, SAMPLE_RATE, 2);
    let cursor = Cursor::new(wav_data);

    let mut decoder =
        DecoderFactory::create_with_probe(cursor, Some("wav"), DecoderConfig::default()).unwrap();

    let total_duration = decoder.duration().unwrap_or(Duration::from_secs(10));
    let total_secs = total_duration.as_secs_f64();

    let mut rng = Xorshift64::new(0xDEAD_BEEF_CAFE_1337);
    let mut successful_seeks = 0u64;

    for i in 0..SEEK_ITERATIONS {
        // Seek to random position (0.01s .. total - 0.5s)
        let max_seek = (total_secs - 0.5).max(0.1);
        let seek_secs = rng.range_f64(0.01, max_seek);
        let seek_pos = Duration::from_secs_f64(seek_secs);

        decoder.seek(seek_pos).unwrap_or_else(|e| {
            panic!("seek #{i} to {seek_secs:.4}s failed: {e}");
        });

        let expected_frame = (seek_secs * SAMPLE_RATE as f64) as u64;

        // Read a burst of chunks and verify consistency
        let mut prev_frame_end: Option<u64> = None;

        for j in 0..CHUNKS_PER_BURST {
            let chunk = match decoder.next_chunk() {
                Ok(Some(c)) => c,
                Ok(None) => break, // EOF
                Err(e) => panic!("next_chunk failed at seek #{i}, burst #{j}: {e}"),
            };

            let meta = chunk.meta;

            // Spec correct
            assert_eq!(meta.spec.sample_rate, SAMPLE_RATE);
            assert_eq!(meta.spec.channels, 2);

            // First chunk after seek: frame_offset should approximate seek target
            if j == 0 {
                let diff = (meta.frame_offset as i64 - expected_frame as i64).unsigned_abs();
                assert!(
                    diff < 4096,
                    "seek #{i}: frame_offset {} too far from expected {} (diff {})",
                    meta.frame_offset,
                    expected_frame,
                    diff
                );
            }

            // Timestamp matches frame_offset / sample_rate
            let expected_ts =
                Duration::from_secs_f64(meta.frame_offset as f64 / meta.spec.sample_rate as f64);
            let ts_diff = if meta.timestamp > expected_ts {
                meta.timestamp - expected_ts
            } else {
                expected_ts - meta.timestamp
            };
            assert!(
                ts_diff < Duration::from_millis(1),
                "seek #{i}, burst #{j}: timestamp drift {ts_diff:?}"
            );

            // Continuity within burst (no gaps)
            if let Some(prev_end) = prev_frame_end {
                assert_eq!(
                    meta.frame_offset, prev_end,
                    "seek #{i}, burst #{j}: frame_offset gap: expected {prev_end}, got {}",
                    meta.frame_offset
                );
            }

            // Progressive: no segment/variant
            assert_eq!(meta.segment_index, None);
            assert_eq!(meta.variant_index, None);
            assert_eq!(meta.epoch, 0);

            prev_frame_end = Some(meta.frame_offset + chunk.frames() as u64);
        }

        successful_seeks += 1;
    }

    assert_eq!(
        successful_seeks, SEEK_ITERATIONS as u64,
        "not all seek iterations completed"
    );
}
