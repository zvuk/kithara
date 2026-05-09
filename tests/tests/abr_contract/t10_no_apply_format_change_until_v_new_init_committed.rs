//! T10 — нет `apply_format_change` до commit V_new init (контракт C-A3).
//!
//! После `set_mode(V_new)` audio FSM не должен пересоздавать декодер
//! до того, как V_new seg-0 c `init_len > 0` зацементирован. Если
//! recreate срабатывает раньше — это означает, что либо
//! `format_change_segment_range` вернул чужой range через fallback,
//! либо `detect_format_change` упал в `.or_else(current_segment_range())`.

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
async fn t10_no_apply_format_change_until_v_new_init_committed(temp_dir: TestTempDir) {
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
        .expect("T10 setup — `build_chunk` >= PRE_SWITCH_TARGET did not fire within 15s.");

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
        .expect("T10 RED — `AbrState::apply(d=V_new)` did not fire within 2s.");
    let apply_seq = apply_event.seq().expect("`apply` event must carry `seq`");

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
        .expect(
            "T10 RED — `commit_segment(V_new, 0, init_len>0)` не сработал в 8s. \
             Scheduler не зафетчил V_new init.",
        );
    let commit_seq = commit_event
        .seq()
        .expect("`commit_segment` event must carry `seq`");

    let early_recreates: Vec<String> = recorder
        .snapshot()
        .into_iter()
        .filter(|e| {
            e.probe_name() == Some("apply_format_change")
                && e.seq().is_some_and(|s| s > apply_seq && s < commit_seq)
        })
        .map(|e| {
            format!(
                "  seq={} target_offset={:?} caller_fn={:?}",
                e.seq().unwrap_or(u64::MAX),
                e.u64("target_offset"),
                e.caller_fn(),
            )
        })
        .collect();

    assert!(
        early_recreates.is_empty(),
        "T10 RED — `apply_format_change` сработал {n} раз в окне \
         (apply.seq={apply_seq}, commit.seq={commit_seq}). Декодер пересоздан до того, \
         как V_new seg-0 init был зацементирован. Причина — либо \
         `format_change_segment_range` вернул чужой range через fallback-цепочку, \
         либо `detect_format_change` упал в `.or_else(current_segment_range())`. \
         События:\n{events}",
        n = early_recreates.len(),
        events = early_recreates.join("\n"),
    );
}
