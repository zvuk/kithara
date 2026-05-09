//! T11 — `apply_format_change.target_offset` совпадает с V_new
//! `init_segment_range_for_variant` `OkSeg0` `range.start` (контракт C-A4).
//!
//! Когда `apply_format_change` срабатывает после commit V_new init,
//! его `target_offset` должен соответствовать byte-offset'у seg-0 init
//! V_new, разрешённому через `init_segment_range_for_variant` с
//! outcome=OkSeg0. Любой другой offset означает, что
//! `format_change_segment_range` вернул чужой range.

use kithara::{assets::StoreOptions, audio::PcmReader, decode::DecoderBackend, hls::AbrMode};
use kithara_platform::time::Duration;
use kithara_test_utils::{TestServerHelper, TestTempDir, probe_capture, temp_dir};

use crate::abr_contract::helpers::{
    Consts,
    params::{open_audio, wave_fixture_4_variants},
};

#[kithara::test(
    tokio,
    native,
    serial,
    timeout(Duration::from_secs(30)),
    env(KITHARA_HANG_TIMEOUT_SECS = "5")
)]
async fn t11_apply_format_change_target_offset_matches_init_range(temp_dir: TestTempDir) {
    let recorder = probe_capture::install();
    let server = TestServerHelper::new().await;
    let created = server
        .create_hls(wave_fixture_4_variants())
        .await
        .expect("create wave HLS fixture");
    let url = created.master_url();
    let store = StoreOptions::new(temp_dir.path());

    let variant_from = Consts::VARIANT_AAC_LQ;
    let variant_to = Consts::VARIANT_AAC_HQ;

    let audio = open_audio(
        &url,
        store,
        AbrMode::Manual(variant_from),
        DecoderBackend::Symphonia,
        3,
    )
    .await;

    let pre_switch_target_us = (Consts::PRE_SWITCH_TARGET_SECS * 1_000_000.0) as u64;

    recorder
        .wait_for_probe(
            |e| {
                e.probe_name() == Some("build_chunk")
                    && e.u64("timestamp")
                        .is_some_and(|ts| ts >= pre_switch_target_us)
            },
            Duration::from_secs(15),
        )
        .expect("T11 setup — `build_chunk` >= PRE_SWITCH_TARGET did not fire within 15s.");

    audio
        .abr_handle()
        .expect("HLS Audio must expose an ABR handle")
        .set_mode(AbrMode::Manual(variant_to))
        .expect("set_mode");

    let apply_event = recorder
        .wait_for_probe(
            |e| e.probe_name() == Some("apply") && e.u64("d") == Some(variant_to as u64),
            Duration::from_secs(2),
        )
        .expect("T11 RED — `AbrState::apply(d=V_new)` не сработал в 2s.");
    let apply_seq = apply_event.seq().expect("apply must carry seq");

    let commit_event = recorder
        .wait_for_probe(
            |e| {
                e.probe_name() == Some("commit_segment")
                    && e.u64("variant") == Some(variant_to as u64)
                    && e.u64("seg_idx") == Some(0)
                    && e.u64("init_len").is_some_and(|l| l > 0)
            },
            Duration::from_secs(8),
        )
        .expect("T11 RED — commit_segment(V_new, 0, init_len>0) не сработал в 8s.");
    let commit_seq = commit_event.seq().expect("commit_segment must carry seq");

    let recreate_event = recorder
        .wait_for_probe(
            |e| {
                e.probe_name() == Some("apply_format_change")
                    && e.seq().is_some_and(|s| s > commit_seq)
            },
            Duration::from_secs(5),
        )
        .expect(
            "T11 RED — `apply_format_change` did not fire within 5s of \
             commit_segment(V_new, 0, init_len>0). Декодер не пересоздан \
             после готовности init.",
        );
    let recreate_offset = recreate_event
        .u64("target_offset")
        .expect("apply_format_change must carry target_offset");

    let last_ok_seg0_for_v_new = recorder
        .snapshot()
        .into_iter()
        .filter(|e| {
            e.probe_name() == Some("init_segment_range_for_variant")
                && e.u64("variant") == Some(variant_to as u64)
                && e.u64("outcome") == Some(0)
        })
        .last();

    let expected_offset = last_ok_seg0_for_v_new
        .as_ref()
        .and_then(|e| e.u64("range_start"))
        .unwrap_or_else(|| {
            let outcomes: Vec<String> = recorder
                .snapshot()
                .into_iter()
                .filter(|e| {
                    e.probe_name() == Some("init_segment_range_for_variant")
                        && e.u64("variant") == Some(variant_to as u64)
                })
                .map(|e| {
                    format!(
                        "  seq={} outcome={:?} committed_count={:?} range_start={:?}",
                        e.seq().unwrap_or(u64::MAX),
                        e.u64("outcome"),
                        e.u64("committed_count"),
                        e.u64("range_start"),
                    )
                })
                .collect();
            panic!(
                "T11 RED — нет ни одного `init_segment_range_for_variant` события \
                 с outcome=OkSeg0(0) для V_new={variant_to}. Resolver всегда возвращал \
                 fallback outcome. apply_format_change.target_offset={recreate_offset} \
                 пришёл из чужой ветки. События resolver'а:\n{outcomes_joined}",
                outcomes_joined = outcomes.join("\n"),
            )
        });

    assert_eq!(
        recreate_offset,
        expected_offset,
        "T11 RED — `apply_format_change.target_offset = {recreate_offset}` \
         не совпадает с V_new init range start = {expected_offset} \
         (`init_segment_range_for_variant(V_new) outcome=OkSeg0`). Recreate с чужим offset \
         означает срабатывание fallback-цепочки до OkSeg0. apply.seq={apply_seq}, \
         commit.seq={commit_seq}, recreate.seq={recreate_seq:?}.",
        recreate_seq = recreate_event.seq()
    );
}
