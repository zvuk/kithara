//! T7 — no double-decode (axiom A9).
//!
//! Once the decoder has emitted a `PcmChunk` carrying
//! `variant_index = Some(variant_to)`, no later chunk may carry
//! `variant_index = Some(variant_from)`. Such a regression would mean
//! the decoder rebuilt against the new variant and then continued to
//! emit frames decoded against the old variant — the textbook
//! double-decode race.
//!
//! Unlike T1 (which calibrates phase across the seam), T7 only walks
//! the per-chunk variant tag — it catches the regression even when
//! audio continuity is otherwise maintained. T7 is independent of T1
//! so a simultaneous failure in both is diagnostic, not redundant.

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
};

#[kithara::test(
    tokio,
    native,
    serial,
    timeout(Duration::from_secs(45)),
    env(KITHARA_HANG_TIMEOUT_SECS = "3")
)]
#[case::lq_to_hq_sw(
    Consts::VARIANT_AAC_LQ,
    Consts::VARIANT_AAC_HQ,
    DecoderBackend::Symphonia
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::lq_to_hq_hw(Consts::VARIANT_AAC_LQ, Consts::VARIANT_AAC_HQ, DecoderBackend::Apple)
)]
#[case::hq_to_lq_sw(
    Consts::VARIANT_AAC_HQ,
    Consts::VARIANT_AAC_LQ,
    DecoderBackend::Symphonia
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::hq_to_lq_hw(Consts::VARIANT_AAC_HQ, Consts::VARIANT_AAC_LQ, DecoderBackend::Apple)
)]
#[case::hq_to_flac_sw(
    Consts::VARIANT_AAC_HQ,
    Consts::VARIANT_FLAC,
    DecoderBackend::Symphonia
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::hq_to_flac_hw(Consts::VARIANT_AAC_HQ, Consts::VARIANT_FLAC, DecoderBackend::Apple)
)]
#[case::flac_to_lq_sw(
    Consts::VARIANT_FLAC,
    Consts::VARIANT_AAC_LQ,
    DecoderBackend::Symphonia
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::flac_to_lq_hw(Consts::VARIANT_FLAC, Consts::VARIANT_AAC_LQ, DecoderBackend::Apple)
)]
async fn t7_no_double_decode(
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
                    if post >= Consts::POST_SWITCH_CHUNKS_REQUIRED_LONG {
                        break;
                    }
                }
            }
            Ok(ChunkOutcome::Pending { .. }) => {
                kithara_platform::time::sleep(Duration::from_millis(5)).await;
            }
            Ok(ChunkOutcome::Eof { .. }) => break,
            Err(e) => panic!("T7 [{variant_from}->{variant_to} {backend:?}]: decode error: {e}"),
        }
    }

    assert!(
        switched,
        "T7 [{variant_from}->{variant_to} {backend:?}]: warmup never reached \
         pre_switch_target {pre_switch_target:?}"
    );
    let first_to = first_to_idx.unwrap_or_else(|| {
        panic!(
            "T7 [{variant_from}->{variant_to} {backend:?}]: switch did not propagate — \
             collected {n} chunks, none on variant_to. Variant trace: {trace:?}",
            n = chunks.len(),
            trace = chunks
                .iter()
                .map(|c| c.meta.variant_index)
                .collect::<Vec<_>>(),
        )
    });

    let regressions: Vec<(usize, &PcmChunk)> = chunks
        .iter()
        .enumerate()
        .skip(first_to + 1)
        .filter(|(_, c)| c.meta.variant_index == Some(variant_from))
        .collect();
    assert!(
        regressions.is_empty(),
        "T7 [{variant_from}->{variant_to} {backend:?}] A9: {n} chunk(s) reverted \
         to variant_from after the first variant_to chunk at index {first_to}: \
         {indices:?}. Variant trace: {trace:?}",
        n = regressions.len(),
        indices = regressions.iter().map(|(idx, _)| *idx).collect::<Vec<_>>(),
        trace = chunks
            .iter()
            .map(|c| c.meta.variant_index)
            .collect::<Vec<_>>(),
    );
}
