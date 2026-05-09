//! T14 — после `set_mode(V_new)` scheduler фетчит V_new по текущему
//! `decode_time`, а не seg-0 (контракт C-A5).
//!
//! Полностью probe-driven: ни одного polling-цикла. Pre-switch warmup —
//! через `wait_for_probe(build_chunk, timestamp >= 4s)`. Каждый шаг ABR
//! цепочки (`on_mode_changed → tick → apply`) — отдельная проба с
//! tight 2-секундным бюджетом и точным сообщением, который link сломан.

use kithara::{assets::StoreOptions, audio::PcmReader, decode::DecoderBackend, hls::AbrMode};
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
    timeout(Duration::from_secs(20)),
    env(KITHARA_HANG_TIMEOUT_SECS = "5")
)]
async fn t14_post_apply_fetch_targets_current_decode_time_not_seg_0(temp_dir: TestTempDir) {
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

    // Ждём пока декодер дойдёт до PRE_SWITCH_TARGET через пробу
    // `build_chunk(timestamp, frames)`. Реально достигнутый timestamp
    // используется ниже для расчёта ожидаемого segment_index.
    let pre_switch_target_us = (Consts::PRE_SWITCH_TARGET_SECS * 1_000_000.0) as u64;
    let warmup_chunk = recorder
        .wait_for_probe(
            |e| {
                e.probe_name() == Some("build_chunk")
                    && e.u64("timestamp")
                        .is_some_and(|ts| ts >= pre_switch_target_us)
            },
            Duration::from_secs(15),
        )
        .expect("T14 setup — `build_chunk` >= PRE_SWITCH_TARGET did not fire within 15s.");
    let last_decode_time_us = warmup_chunk
        .u64("timestamp")
        .expect("build_chunk must carry timestamp");

    // Триггер switch.
    audio
        .abr_handle()
        .expect("HLS Audio must expose an ABR handle")
        .set_mode(AbrMode::Manual(variant_to))
        .expect("set_mode");

    // ABR chain: каждый link — отдельная проба, отдельный panic.
    let on_mode_changed = recorder
        .wait_for_probe(
            |e| {
                e.probe_name() == Some("on_mode_changed")
                    && e.u64("mode") == Some(variant_to as u64)
            },
            Duration::from_secs(2),
        )
        .expect(
            "T14 RED — `AbrController::on_mode_changed` did not fire with \
             target mode within 2s of `AbrHandle::set_mode`.",
        );

    let tick_after_mode = recorder
        .wait_for_probe(
            |e| {
                e.probe_name() == Some("tick")
                    && e.seq()
                        .zip(on_mode_changed.seq())
                        .is_some_and(|(s, m)| s > m)
                    && e.u64("peer_id") == on_mode_changed.u64("peer_id")
            },
            Duration::from_secs(2),
        )
        .expect(
            "T14 RED — `on_mode_changed` fired but `Controller::tick` did \
             not run for the same peer within 2s.",
        );

    let apply_event = recorder
        .wait_for_probe(
            |e| {
                e.probe_name() == Some("apply")
                    && e.u64("d") == Some(variant_to as u64)
                    && e.seq()
                        .zip(tick_after_mode.seq())
                        .is_some_and(|(s, t)| s > t)
            },
            Duration::from_secs(2),
        )
        .expect(
            "T14 RED — `tick` ran but `AbrState::apply` was not called \
             for the target variant within 2s.",
        );

    let apply_seq = apply_event.seq().expect("`apply` event must carry `seq`");

    // Ждём первый `emit_fetch_cmd(V_new, plan_need_init=1)`.
    let fetch_event = recorder
        .wait_for_probe(
            |e| {
                e.probe_name() == Some("emit_fetch_cmd")
                    && e.u64("variant") == Some(variant_to as u64)
                    && e.u64("plan_need_init") == Some(1)
            },
            Duration::from_secs(3),
        )
        .unwrap_or_else(|| {
            let post_apply: Vec<String> = recorder
                .snapshot()
                .into_iter()
                .filter(|e| e.seq().is_some_and(|s| s > apply_seq))
                .take(40)
                .map(|e| {
                    format!(
                        "  seq={} probe={:?} caller_fn={:?} variant={:?} segment_index={:?} plan_need_init={:?}",
                        e.seq().unwrap_or(u64::MAX),
                        e.probe_name(),
                        e.caller_fn(),
                        e.u64("variant"),
                        e.u64("segment_index"),
                        e.u64("plan_need_init"),
                    )
                })
                .collect();
            panic!(
                "T14 RED — scheduler did not emit `plan_need_init=1` for \
                 V_new={variant_to} within 3s of ABR commit (apply.seq={apply_seq}).\n\
                 Probe events recorded after apply (most recent {n} shown):\n{events}",
                n = post_apply.len(),
                events = post_apply.join("\n"),
            )
        });

    let fetch_seq = fetch_event
        .seq()
        .expect("`emit_fetch_cmd` event must carry `seq`");
    assert!(
        fetch_seq > apply_seq,
        "fetch.seq ({fetch_seq}) must be strictly greater than apply.seq \
         ({apply_seq})"
    );

    let caller_fn = fetch_event.caller_fn().unwrap_or_else(|| {
        panic!(
            "`emit_fetch_cmd` must carry `caller_fn`. Event: {evt:?}",
            evt = fetch_event
        )
    });
    assert!(
        caller_fn.contains("kithara_hls::peer"),
        "`emit_fetch_cmd` fired from unexpected caller_fn: {caller_fn:?}. \
         Expected a frame inside `kithara_hls::peer::*`."
    );

    // RED-сердцевина: segment_index должен соответствовать playback
    // decode_time, а не seg-0.
    let segment_index = fetch_event
        .u64("segment_index")
        .expect("emit_fetch_cmd carries segment_index");
    let last_decode_secs = (last_decode_time_us as f64) / 1_000_000.0;
    let expected_segment = (last_decode_secs / Consts::SEGMENT_DURATION_SECS) as u64;
    let drift = segment_index.abs_diff(expected_segment);
    assert!(
        drift <= 1,
        "T14 RED — `plan_need_init` fetch for V_new ({variant_to}) \
         targeted seg={segment_index}, но playback decode_time at apply был \
         {last_decode_secs:.2}s → ожидался seg≈{expected_segment} \
         (segment_duration_secs = {seg_dur}). Drift = {drift}. \
         Scheduler выбрал cursor из V_old's reader_floor вместо \
         playback-time mapping V_new; новый декодер рестартует с V_new seg-0.",
        seg_dur = Consts::SEGMENT_DURATION_SECS,
    );
}
