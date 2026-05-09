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
use kithara_platform::time::Duration;
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
    let recorder = probe_capture::install();
    let server = TestServerHelper::new().await;
    let created = server
        .create_hls(wave_fixture_4_variants())
        .await
        .expect("create wave HLS fixture");
    let url = created.master_url();
    let store = StoreOptions::new(temp_dir.path());

    let label = format!("{variant_from}->{variant_to} {backend:?}");
    let mut audio = open_audio(&url, store, AbrMode::Manual(variant_from), backend, 3).await;

    // Warmup — wait_for_probe(build_chunk, ts >= PRE_SWITCH_TARGET).
    let pre_switch_target_us = (Consts::PRE_SWITCH_TARGET_SECS * 1_000_000.0) as u64;
    recorder
        .wait_for_probe(
            |e| {
                e.probe_name() == Some("build_chunk")
                    && e.u64("timestamp")
                        .is_some_and(|ts| ts >= pre_switch_target_us)
            },
            Duration::from_secs(20),
        )
        .unwrap_or_else(|| {
            panic!("T7 [{label}]: warmup `build_chunk` >= PRE_SWITCH_TARGET not seen in 20s")
        });

    audio
        .abr_handle()
        .expect("HLS Audio must expose an ABR handle")
        .set_mode(AbrMode::Manual(variant_to))
        .expect("set_mode");

    // Ждём пересоздание декодера — сигнал, что V_new chunks начались.
    let recreate_event = recorder
        .wait_for_probe(
            |e| e.probe_name() == Some("apply_format_change"),
            Duration::from_secs(15),
        )
        .unwrap_or_else(|| panic!("T7 [{label}]: apply_format_change not seen in 15s of set_mode"));
    let recreate_seq = recreate_event
        .seq()
        .expect("apply_format_change must carry seq");

    // Ждём POST_SWITCH_CHUNKS_REQUIRED_LONG штук `build_chunk` после
    // recreate, чтобы регрессия (если есть) гарантированно произошла.
    let mut last_seq = recreate_seq;
    for i in 0..Consts::POST_SWITCH_CHUNKS_REQUIRED_LONG {
        let evt = recorder
            .wait_for_probe(
                |e| e.probe_name() == Some("build_chunk") && e.seq().is_some_and(|s| s > last_seq),
                Duration::from_secs(5),
            )
            .unwrap_or_else(|| {
                panic!(
                    "T7 [{label}]: build_chunk #{i} after apply_format_change \
                     (recreate.seq={recreate_seq}) did not fire within 5s."
                )
            });
        last_seq = evt.seq().expect("build_chunk must carry seq");
    }

    // Сливаем chunks через next_chunk и проверяем variant_index.
    let mut chunks: Vec<PcmChunk> = Vec::new();
    loop {
        match PcmReader::next_chunk(&mut audio) {
            Ok(ChunkOutcome::Chunk(chunk)) => chunks.push(chunk),
            Ok(ChunkOutcome::Pending { .. }) | Ok(ChunkOutcome::Eof { .. }) => break,
            Err(e) => panic!("T7 [{label}]: drain decode error: {e}"),
        }
    }

    let first_to = chunks
        .iter()
        .position(|c| c.meta.variant_index == Some(variant_to))
        .unwrap_or_else(|| {
            panic!(
                "T7 [{label}]: switch did not propagate — collected {n} chunks, \
                 none on variant_to. Variant trace: {trace:?}",
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
        "T7 [{label}] A9: {n} chunk(s) reverted to variant_from after the first \
         variant_to chunk at index {first_to}: {indices:?}. Variant trace: {trace:?}",
        n = regressions.len(),
        indices = regressions.iter().map(|(idx, _)| *idx).collect::<Vec<_>>(),
        trace = chunks
            .iter()
            .map(|c| c.meta.variant_index)
            .collect::<Vec<_>>(),
    );
}
