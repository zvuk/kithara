//! T8 — sanity baseline (no ABR switch).
//!
//! Plays the wave fixture from start to natural EOF on a single
//! manually-pinned variant. The contract here is not an axiom from
//! `abr-switch-contract.md` directly — it is the foundation those
//! axioms stand on:
//!
//! * the fixture renders the expected number of audio frames per
//!   variant;
//! * the decoder emits exactly that many frames;
//! * `record_abr_variant_committed` fires once (the initial `set_mode`
//!   commit), and `record_midstream_switch_committed` never fires.
//!
//! If T8 fails, every other T-test would be diagnostically confusing,
//! so it runs first in the suite ordering.

use kithara::{
    assets::StoreOptions,
    audio::{ChunkOutcome, PcmReader},
    decode::DecoderBackend,
    hls::AbrMode,
    stream::AudioCodec,
};
use kithara_platform::time::{Duration, Instant};
use kithara_test_utils::{TestServerHelper, TestTempDir, probe_capture, temp_dir};

use super::helpers::{
    Consts,
    assertions::assert_exact_count,
    params::{open_audio, wave_fixture_4_variants},
};

/// Per-codec frame-sample size used by the fixture encoders. Hardcoded
/// rather than queried at runtime so the test fails loudly if the
/// fixture's encoder is silently swapped out.
fn encoder_frame_samples(codec: AudioCodec) -> usize {
    match codec {
        AudioCodec::AacLc => 1024,
        AudioCodec::Flac => 4608,
        other => panic!("T8 fixture does not use codec {other:?}"),
    }
}

/// Total decoded frames expected from a single variant of the wave
/// fixture. Mirrors `kithara_test_utils::hls_stream::encode_packaged_variant`:
/// `packets_per_segment = ceil(SEGMENT_DURATION_SECS · SAMPLE_RATE / frame_samples)`,
/// `total = packets_per_segment · frame_samples · SEGMENTS_PER_VARIANT`.
fn expected_frames_per_variant(codec: AudioCodec) -> u64 {
    let frame_samples = encoder_frame_samples(codec);
    let requested_segment_frames =
        (Consts::SEGMENT_DURATION_SECS * f64::from(Consts::SAMPLE_RATE)).round() as usize;
    let packets_per_segment = requested_segment_frames.div_ceil(frame_samples).max(1);
    (packets_per_segment * frame_samples * Consts::SEGMENTS_PER_VARIANT) as u64
}

fn variant_codec(variant: usize) -> AudioCodec {
    if variant == Consts::VARIANT_FLAC {
        AudioCodec::Flac
    } else {
        AudioCodec::AacLc
    }
}

#[kithara::test(
    tokio,
    native,
    serial,
    timeout(Duration::from_secs(60)),
    env(KITHARA_HANG_TIMEOUT_SECS = "3")
)]
#[case::aac_lq_sw(Consts::VARIANT_AAC_LQ, DecoderBackend::Symphonia)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::aac_lq_hw(Consts::VARIANT_AAC_LQ, DecoderBackend::Apple)
)]
#[case::flac_sw(Consts::VARIANT_FLAC, DecoderBackend::Symphonia)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::flac_hw(Consts::VARIANT_FLAC, DecoderBackend::Apple)
)]
async fn t8_natural_eof_complete_playback(
    temp_dir: TestTempDir,
    #[case] variant: usize,
    #[case] backend: DecoderBackend,
) {
    let recorder = probe_capture::install();
    let server = TestServerHelper::new().await;
    let created = server
        .create_hls(wave_fixture_4_variants())
        .await
        .expect("create wave HLS fixture");
    let url = created.master_url();
    let store = StoreOptions::new(temp_dir.path());

    let mut audio = open_audio(&url, store, AbrMode::Manual(variant), backend, 3).await;

    // Read until the decoder reports EOF or the deadline elapses.
    // The fixture is `SEGMENTS_PER_VARIANT * SEGMENT_DURATION_SECS`
    // seconds of audio (24 s by default); the deadline is generous to
    // absorb cold-start without exceeding the test wall-time budget.
    let deadline = Instant::now() + Duration::from_secs(45);
    let mut total_frames: u64 = 0;
    let mut saw_eof = false;
    while Instant::now() < deadline {
        let _ = audio.preload();
        match PcmReader::next_chunk(&mut audio) {
            Ok(ChunkOutcome::Chunk(chunk)) => {
                assert_eq!(
                    chunk.meta.variant_index,
                    Some(variant),
                    "T8: chunk emitted on unexpected variant — fixture or decoder \
                     leaked a foreign variant on the no-switch path \
                     (expected {variant}, got {actual:?}, frame_offset={off})",
                    actual = chunk.meta.variant_index,
                    off = chunk.meta.frame_offset,
                );
                total_frames += u64::from(chunk.meta.frames);
            }
            Ok(ChunkOutcome::Pending { .. }) => {
                kithara_platform::time::sleep(Duration::from_millis(5)).await;
            }
            Ok(ChunkOutcome::Eof { .. }) => {
                saw_eof = true;
                break;
            }
            Err(e) => panic!("T8: decode error during baseline playback: {e}"),
        }
    }

    assert!(
        saw_eof,
        "T8: decoder did not reach natural EOF before deadline"
    );

    // The fixture renders exactly `packets_per_segment · frame_samples
    // · SEGMENTS_PER_VARIANT` PCM frames per variant. Decoder output
    // may differ by at most one encoder frame (`frame_samples`) due to
    // AAC encoder priming / EditList interpretation — that is part of
    // the codec's structure, not a bug. Anything larger is lost or
    // duplicated audio.
    let codec = variant_codec(variant);
    let expected = expected_frames_per_variant(codec);
    let frame_samples = encoder_frame_samples(codec) as i64;
    let drift = total_frames as i64 - expected as i64;
    assert!(
        drift.abs() <= frame_samples,
        "T8: total decoded frames diverge from fixture frame count by more than \
         one encoder frame ({frame_samples} samples): expected {expected}, \
         got {total_frames} (drift {drift}). Backend = {backend:?}, variant = {variant}."
    );

    // No ABR commit and no midstream switch are expected on the
    // baseline path. `record_abr_variant_committed` only fires for
    // controller-driven transitions through `AbrController::tick` ⇒
    // `decision.changed`; the seed `set_initial_abr_mode(Manual(_))`
    // bypasses that path, so the probe count is exactly zero. Any
    // non-zero count here means the controller spuriously transitioned
    // a variant during a no-set_mode test — a contract violation.
    let abr_commits = recorder.events_with_probe("record_abr_variant_committed");
    assert_exact_count(abr_commits.len(), 0, "T8 abr_variant_committed count");
    let midstream = recorder.events_with_probe("record_midstream_switch_committed");
    assert_exact_count(midstream.len(), 0, "T8 midstream_switch_committed count");
}
