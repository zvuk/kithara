//! Cross-decoder protocol contract.
//!
//! Validates that every `InnerDecoder` backend exposes the same interface
//! contract on the same input: spec, duration, total frames, seek-then-resume,
//! and end-of-stream. On macOS with the `apple` feature enabled the test
//! also compares PCM output between Symphonia and Apple within an L2-norm
//! tolerance — catching silent divergences between the software and
//! hardware paths.
//!
//! Android is not exercised here because `MediaCodec` is only available on
//! Android targets; the Android backend's capability matrix has its own
//! unit tests in `android/format.rs`.

use std::{io::Cursor, time::Duration};

use kithara_decode::{DecoderConfig, DecoderFactory, InnerDecoder};
use kithara_stream::{AudioCodec, ContainerFormat, MediaInfo};
use kithara_test_utils::kithara;

const TEST_MP3_BYTES: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../assets/test.mp3"
));

/// Backend selector for cross-decoder comparison.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Backend {
    Symphonia,
    #[cfg(all(feature = "apple", any(target_os = "macos", target_os = "ios")))]
    Apple,
}

impl Backend {
    fn make_mp3(self) -> Box<dyn InnerDecoder> {
        let source = Cursor::new(TEST_MP3_BYTES.to_vec());
        let media_info = MediaInfo::new(Some(AudioCodec::Mp3), Some(ContainerFormat::MpegAudio));
        match self {
            Self::Symphonia => {
                let config = DecoderConfig {
                    prefer_hardware: false,
                    ..Default::default()
                };
                DecoderFactory::create_from_media_info(source, &media_info, &config)
                    .expect("symphonia mp3 decoder should create")
            }
            #[cfg(all(feature = "apple", any(target_os = "macos", target_os = "ios")))]
            Self::Apple => {
                let config = DecoderConfig {
                    prefer_hardware: true,
                    ..Default::default()
                };
                DecoderFactory::create_from_media_info(source, &media_info, &config)
                    .expect("apple mp3 decoder should create")
            }
        }
    }
}

fn available_backends() -> Vec<Backend> {
    #[cfg(all(feature = "apple", any(target_os = "macos", target_os = "ios")))]
    {
        vec![Backend::Symphonia, Backend::Apple]
    }
    #[cfg(not(all(feature = "apple", any(target_os = "macos", target_os = "ios"))))]
    {
        vec![Backend::Symphonia]
    }
}

/// Drain the decoder and return concatenated f32 PCM samples.
fn drain_all(decoder: &mut dyn InnerDecoder) -> Vec<f32> {
    let mut all = Vec::new();
    while let Some(chunk) = decoder.next_chunk().expect("decode should succeed") {
        all.extend_from_slice(chunk.samples());
    }
    all
}

/// L2 norm of a PCM slice.
#[cfg(all(feature = "apple", any(target_os = "macos", target_os = "ios")))]
fn l2_norm(samples: &[f32]) -> f64 {
    samples
        .iter()
        .map(|s| f64::from(*s * *s))
        .sum::<f64>()
        .sqrt()
}

#[kithara::test]
fn spec_after_create_is_consistent_across_backends() {
    let specs: Vec<_> = available_backends()
        .into_iter()
        .map(|b| (b, b.make_mp3().spec()))
        .collect();

    let first = specs[0].1;
    for (backend, spec) in &specs[1..] {
        assert_eq!(
            *spec, first,
            "spec mismatch between {:?} and {:?}: {:?} vs {:?}",
            specs[0].0, backend, first, spec
        );
    }
}

#[kithara::test]
fn duration_after_create_is_consistent_across_backends() {
    let durations: Vec<_> = available_backends()
        .into_iter()
        .map(|b| (b, b.make_mp3().duration()))
        .collect();

    // All backends should report *some* duration for a complete local file.
    for (backend, duration) in &durations {
        let d = duration.unwrap_or_else(|| {
            panic!("backend {backend:?} must report duration for a complete MP3 file")
        });
        assert!(
            d.as_secs_f64() > 1.0,
            "duration from {backend:?} is suspiciously short: {d:?}"
        );
    }

    // Cross-backend agreement within ±5% (priming/padding differences).
    let first = durations[0].1.expect("symphonia reports duration");
    for (backend, duration) in &durations[1..] {
        let other = duration.expect("backend reports duration");
        let diff = (first.as_secs_f64() - other.as_secs_f64()).abs();
        let tol = first.as_secs_f64() * 0.05;
        assert!(
            diff <= tol,
            "duration diverges between Symphonia ({:?}) and {:?} ({:?}); diff={:.3}s, tol={:.3}s",
            first,
            backend,
            other,
            diff,
            tol
        );
    }
}

#[kithara::test]
fn total_frames_are_consistent_across_backends() {
    let frame_counts: Vec<_> = available_backends()
        .into_iter()
        .map(|b| {
            let mut dec = b.make_mp3();
            let samples = drain_all(&mut *dec);
            let channels = dec.spec().channels.max(1) as usize;
            let frames = samples.len() / channels;
            (b, frames)
        })
        .collect();

    let first = frame_counts[0].1;
    for (backend, frames) in &frame_counts[1..] {
        // Priming/padding differences are typically a few hundred frames; cap
        // at 1% of the shorter stream to guard against a real truncation bug.
        let tol = first.max(*frames) / 100;
        let diff = first.abs_diff(*frames);
        assert!(
            diff <= tol,
            "frame count mismatch: {:?}={}, {:?}={}, diff={} > tol={}",
            frame_counts[0].0,
            first,
            backend,
            frames,
            diff,
            tol
        );
    }
}

#[kithara::test]
fn seek_then_first_chunk_timestamp_is_after_target() {
    const TARGET: Duration = Duration::from_millis(500);
    let tol = Duration::from_millis(200);

    for backend in available_backends() {
        let mut dec = backend.make_mp3();
        dec.seek(TARGET).expect("seek should succeed");
        let chunk = dec
            .next_chunk()
            .expect("next_chunk after seek")
            .expect("at least one chunk after a 0.5s seek");

        // Apple seek is byte-estimated, so ts may land slightly before the
        // requested target (but should still be close). Symphonia accurate
        // seek lands at or after target.
        let ts = chunk.meta.timestamp;
        assert!(
            ts + tol >= TARGET,
            "{:?}: first chunk after seek at {:?} is earlier than target {:?} - tol {:?}",
            backend,
            ts,
            TARGET,
            tol
        );
    }
}

#[kithara::test]
fn end_of_stream_returns_none_repeatedly() {
    for backend in available_backends() {
        let mut dec = backend.make_mp3();
        while dec
            .next_chunk()
            .expect("decode before EOF should succeed")
            .is_some()
        {}

        // Calling again should keep returning None without panicking.
        for _ in 0..3 {
            let next = dec.next_chunk().expect("decode after EOF");
            assert!(
                next.is_none(),
                "backend {backend:?} must keep returning None at EOF"
            );
        }
    }
}

#[cfg(all(feature = "apple", any(target_os = "macos", target_os = "ios")))]
#[kithara::test]
fn full_decode_l2_norm_matches_within_tolerance() {
    // Relative L2-norm tolerance. Both backends decode the same compressed
    // MP3 stream; priming/padding differences shift a few hundred samples
    // but the overall energy should agree within a couple of percent.
    const TOL_REL: f64 = 0.02;

    let mut sym = Backend::Symphonia.make_mp3();
    let mut apl = Backend::Apple.make_mp3();

    let sym_samples = drain_all(&mut *sym);
    let apl_samples = drain_all(&mut *apl);

    let sym_l2 = l2_norm(&sym_samples);
    let apl_l2 = l2_norm(&apl_samples);

    let reference = sym_l2.max(apl_l2);
    assert!(reference > 0.0, "decoded PCM must have non-zero energy");

    let rel = (sym_l2 - apl_l2).abs() / reference;
    assert!(
        rel <= TOL_REL,
        "L2-norm diverges between Symphonia ({:.3}) and Apple ({:.3}); rel={:.4} > tol={}",
        sym_l2,
        apl_l2,
        rel,
        TOL_REL
    );
}
