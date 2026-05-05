//! T1 — phase-continuity wave (axioms A1, A9, A10).
//!
//! Plays the wave fixture, performs ONE manual switch from
//! `variant_from` to `variant_to` mid-stream, then asserts:
//!
//! * **A10** — `meta.frame_offset` is strictly contiguous across every
//!   chunk pair: `c[i+1].frame_offset == c[i].frame_offset + c[i].frames`.
//! * **A9**  — once a `variant_to` chunk has been emitted, no later
//!   chunk reverts to `variant_from`.
//! * **A1**  — the 440 Hz sine wave's phase is continuous across the
//!   seam to within one sample period at the test's sample rate. The
//!   PRE-switch chunks calibrate `phase_offset_decoder` (the static
//!   AAC/FLAC pipeline phase shift); POST-switch chunks must hold the
//!   same offset within ε. Any larger drift is reported in the panic
//!   message as the exact number of samples lost or repeated.
//!
//! No escape hatches: if the switch never propagates, the test
//! panics with the chunk variant log so the failure mode is concrete.

use kithara::{
    assets::StoreOptions,
    audio::{ChunkOutcome, PcmReader},
    decode::{DecoderBackend, PcmChunk},
    hls::AbrMode,
};
use kithara_platform::time::{Duration, Instant};
use kithara_test_utils::{TestServerHelper, TestTempDir, probe_capture, temp_dir};

use super::helpers::{
    Consts,
    params::{open_audio, wave_fixture_4_variants},
    phase_oracle::{expected_phase, measured_phase, phase_delta_in_samples, phase_diff_signed},
};

#[kithara::test(
    tokio,
    native,
    serial,
    timeout(Duration::from_secs(45)),
    env(KITHARA_HANG_TIMEOUT_SECS = "3")
)]
#[case::aac_lq_to_aac_hq_sw(
    Consts::VARIANT_AAC_LQ,
    Consts::VARIANT_AAC_HQ,
    DecoderBackend::Symphonia
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::aac_lq_to_aac_hq_hw(
        Consts::VARIANT_AAC_LQ,
        Consts::VARIANT_AAC_HQ,
        DecoderBackend::Apple
    )
)]
#[case::aac_hq_to_aac_lq_sw(
    Consts::VARIANT_AAC_HQ,
    Consts::VARIANT_AAC_LQ,
    DecoderBackend::Symphonia
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::aac_hq_to_aac_lq_hw(
        Consts::VARIANT_AAC_HQ,
        Consts::VARIANT_AAC_LQ,
        DecoderBackend::Apple
    )
)]
#[case::aac_hq_to_flac_sw(
    Consts::VARIANT_AAC_HQ,
    Consts::VARIANT_FLAC,
    DecoderBackend::Symphonia
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::aac_hq_to_flac_hw(Consts::VARIANT_AAC_HQ, Consts::VARIANT_FLAC, DecoderBackend::Apple)
)]
#[case::flac_to_aac_lq_sw(
    Consts::VARIANT_FLAC,
    Consts::VARIANT_AAC_LQ,
    DecoderBackend::Symphonia
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::flac_to_aac_lq_hw(Consts::VARIANT_FLAC, Consts::VARIANT_AAC_LQ, DecoderBackend::Apple)
)]
async fn t1_phase_continuity_wave(
    temp_dir: TestTempDir,
    #[case] variant_from: usize,
    #[case] variant_to: usize,
    #[case] backend: DecoderBackend,
) {
    let _recorder = probe_capture::install();
    let server = TestServerHelper::new().await;
    let created = server
        .create_hls(wave_fixture_4_variants())
        .await
        .expect("create wave HLS fixture");
    let url = created.master_url();
    let store = StoreOptions::new(temp_dir.path());

    let mut audio = open_audio(&url, store, AbrMode::Manual(variant_from), backend, 3).await;

    let pre_switch_target = Duration::from_secs_f64(Consts::PRE_SWITCH_TARGET_SECS);
    let mut chunks: Vec<PcmChunk> = Vec::new();
    let mut switched = false;
    let mut first_to_idx: Option<usize> = None;

    let deadline = Instant::now() + Duration::from_secs(35);
    while Instant::now() < deadline {
        let _ = audio.preload();
        match PcmReader::next_chunk(&mut audio) {
            Ok(ChunkOutcome::Chunk(chunk)) => {
                let v = chunk.meta.variant_index;
                let ts = chunk.meta.timestamp;
                chunks.push(chunk);
                if !switched && v == Some(variant_from) && ts >= pre_switch_target {
                    audio
                        .abr_handle()
                        .expect("HLS Audio must expose an ABR handle")
                        .set_mode(AbrMode::Manual(variant_to))
                        .expect("set_mode");
                    switched = true;
                }
                if switched && v == Some(variant_to) && first_to_idx.is_none() {
                    first_to_idx = Some(chunks.len() - 1);
                }
                if let Some(start) = first_to_idx {
                    let post = chunks.len() - start;
                    if post >= Consts::POST_SWITCH_CHUNKS_REQUIRED_SHORT {
                        break;
                    }
                }
            }
            Ok(ChunkOutcome::Pending { .. }) => {
                kithara_platform::time::sleep(Duration::from_millis(5)).await;
            }
            Ok(ChunkOutcome::Eof { .. }) => break,
            Err(e) => panic!("T1 [{variant_from}->{variant_to} {backend:?}]: decode error: {e}"),
        }
    }

    assert!(
        switched,
        "T1 [{variant_from}->{variant_to} {backend:?}]: warmup never reached \
         pre_switch_target {pre_switch_target:?}; collected variants: {observed:?}",
        observed = chunks
            .iter()
            .map(|c| c.meta.variant_index)
            .collect::<Vec<_>>(),
    );

    let first_to = first_to_idx.unwrap_or_else(|| {
        panic!(
            "T1 [{variant_from}->{variant_to} {backend:?}]: switch did not propagate — \
             collected {n} chunks, none on variant_to. Variant trace: {trace:?}",
            n = chunks.len(),
            trace = chunks
                .iter()
                .map(|c| c.meta.variant_index)
                .collect::<Vec<_>>(),
        )
    });

    // -------- A10: frame_offset contiguity --------
    for window in chunks.windows(2) {
        let prev = &window[0];
        let next = &window[1];
        let expected = prev
            .meta
            .frame_offset
            .saturating_add(u64::from(prev.meta.frames));
        assert_eq!(
            next.meta.frame_offset,
            expected,
            "T1 [{variant_from}->{variant_to} {backend:?}] A10: frame_offset not \
             contiguous: prev={pf} (+{pframes} frames) ⇒ expected next.frame_offset={expected}, \
             actual={af}. Drift = {drift} frames.",
            pf = prev.meta.frame_offset,
            pframes = prev.meta.frames,
            af = next.meta.frame_offset,
            drift = next.meta.frame_offset as i64 - expected as i64,
        );
    }

    // -------- A9: no double-decode --------
    for (idx, chunk) in chunks.iter().enumerate().skip(first_to) {
        assert_ne!(
            chunk.meta.variant_index,
            Some(variant_from),
            "T1 [{variant_from}->{variant_to} {backend:?}] A9: chunk #{idx} fell back \
             to variant_from after the first variant_to chunk at #{first_to}. \
             frame_offset={off}, variants[0..idx]={trace:?}",
            off = chunk.meta.frame_offset,
            trace = chunks
                .iter()
                .take(idx + 1)
                .map(|c| c.meta.variant_index)
                .collect::<Vec<_>>(),
        );
    }

    // -------- A1: phase continuity --------
    // Calibrate the static decoder phase offset on the LAST pre-switch chunk
    // (closest to the seam; minimises drift accumulation).
    let pre_chunk = &chunks[first_to.checked_sub(1).expect(
        "T1: cannot calibrate phase — no pre-switch chunk before the first variant_to chunk",
    )];
    let post_chunk = &chunks[first_to];

    let pre_measured = measured_phase(
        &pre_chunk.pcm,
        pre_chunk.meta.spec.channels,
        0,
        pre_chunk.meta.spec.sample_rate,
        Consts::SINE_FREQ_HZ,
    );
    let post_measured = measured_phase(
        &post_chunk.pcm,
        post_chunk.meta.spec.channels,
        0,
        post_chunk.meta.spec.sample_rate,
        Consts::SINE_FREQ_HZ,
    );
    let pre_measured = pre_measured.unwrap_or_else(|| {
        panic!(
            "T1 [{variant_from}->{variant_to} {backend:?}] A1: pre-switch chunk \
             magnitude below silence floor — fixture or decoder produced \
             near-zero samples right before the seam. frame_offset={off}",
            off = pre_chunk.meta.frame_offset,
        )
    });
    let post_measured = post_measured.unwrap_or_else(|| {
        panic!(
            "T1 [{variant_from}->{variant_to} {backend:?}] A1: first post-switch \
             chunk magnitude below silence floor — decoder dropped audio across the \
             seam. frame_offset={off}, variant={v:?}",
            off = post_chunk.meta.frame_offset,
            v = post_chunk.meta.variant_index,
        )
    });

    let pre_expected = expected_phase(
        pre_chunk.meta.frame_offset,
        pre_chunk.meta.spec.sample_rate,
        Consts::SINE_FREQ_HZ,
    );
    let post_expected = expected_phase(
        post_chunk.meta.frame_offset,
        post_chunk.meta.spec.sample_rate,
        Consts::SINE_FREQ_HZ,
    );

    let pre_offset = phase_diff_signed(pre_measured, pre_expected);
    let post_offset = phase_diff_signed(post_measured, post_expected);
    let seam_drift = phase_diff_signed(post_offset, pre_offset);

    let sample_period_eps =
        std::f64::consts::TAU * Consts::SINE_FREQ_HZ / f64::from(Consts::SAMPLE_RATE);
    let drift_samples = phase_delta_in_samples(
        seam_drift,
        post_chunk.meta.spec.sample_rate,
        Consts::SINE_FREQ_HZ,
    );

    assert!(
        seam_drift.abs() < sample_period_eps,
        "T1 [{variant_from}->{variant_to} {backend:?}] A1: phase drift across the \
         seam exceeds one sample period (ε = {sample_period_eps} rad). \
         pre_offset = {pre_offset} rad, post_offset = {post_offset} rad, \
         seam_drift = {seam_drift} rad ≈ {drift_samples:.2} samples \
         {direction}. \
         pre.frame_offset = {pf} (variant {pv:?}), \
         post.frame_offset = {pof} (variant {pov:?}).",
        direction = if drift_samples >= 0.0 {
            "lost"
        } else {
            "repeated"
        },
        pf = pre_chunk.meta.frame_offset,
        pv = pre_chunk.meta.variant_index,
        pof = post_chunk.meta.frame_offset,
        pov = post_chunk.meta.variant_index,
    );
}
