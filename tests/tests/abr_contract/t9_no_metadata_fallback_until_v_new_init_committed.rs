//! T9 — нет metadata-fallback'а до commit V_new init (контракт C-A2).
//!
//! После `set_mode(V_new)` audio FSM не должен вызывать
//! `metadata_range_for_segment(variant=V_new, segment_index=0)` пока
//! V_new seg-0 c `init_len > 0` не закоммичен. Любой такой вызов в окне
//! (apply.seq, commit.seq) — это срабатывание fallback-цепочки в
//! `HlsSource::format_change_segment_range`.
//!
//! Тест полностью probe-driven: ни одного polling-цикла в главном
//! потоке. Декодер запускается через увеличенный pcm_buffer_chunks
//! (см. `open_audio` в `helpers.rs`) и наполняет канал автономно —
//! пробы фиксируются на producer-стороне, без участия consumer'а.
//!
//! Пробы:
//! - `build_chunk(timestamp, frames)` — kithara-decode/composed.rs;
//!   сигнал «декодер дошёл до timestamp».
//! - `apply(d)` — kithara-abr; ABR commit.
//! - `commit_segment(variant, seg_idx, init_len)` — kithara-hls/scheduler.
//! - `metadata_range_for_segment(seek_epoch, segment_index, offset)` —
//!   kithara-hls/source.

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
async fn t9_no_metadata_fallback_until_v_new_init_committed(temp_dir: TestTempDir) {
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

    // PRE_SWITCH_TARGET_SECS в микросекундах — `IntoProbeArg for Duration`
    // выводит микросекунды.
    let pre_switch_target_us = (Consts::PRE_SWITCH_TARGET_SECS * 1_000_000.0) as u64;

    // Ждём пока декодер выпустит chunk с timestamp >= 4s — это сигнал,
    // что playback дошёл до точки midstream switch'а.
    recorder
        .wait_for_probe(
            |e| {
                e.probe_name() == Some("build_chunk")
                    && e.u64("timestamp")
                        .is_some_and(|ts| ts >= pre_switch_target_us)
            },
            Duration::from_secs(15),
        )
        .expect(
            "T9 setup — `build_chunk` event with timestamp >= PRE_SWITCH_TARGET \
             did not fire within 15s. Decoder не дошёл до точки переключения.",
        );

    // Триггер switch.
    audio
        .abr_handle()
        .expect("HLS Audio must expose an ABR handle")
        .set_mode(AbrMode::Manual(variant_to))
        .expect("set_mode");

    // Ждём ABR apply(d=V_new).
    let apply_event = recorder
        .wait_for_probe(
            |e| e.probe_name() == Some("apply") && e.u64("d") == Some(variant_to as u64),
            Duration::from_secs(2),
        )
        .expect(
            "T9 RED — `AbrState::apply(d=V_new)` did not fire within 2s of \
             `set_mode`. Earlier link in the ABR chain is broken.",
        );
    let apply_seq = apply_event.seq().expect("`apply` event must carry `seq`");

    // Ждём commit V_new seg-0 init.
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
        .unwrap_or_else(|| {
            let dump: Vec<String> = recorder
                .snapshot()
                .into_iter()
                .filter(|e| e.seq().is_some_and(|s| s > apply_seq))
                .take(80)
                .map(|e| {
                    format!(
                        "  seq={} probe={:?} variant={:?} seg_idx={:?} segment_index={:?} init_len={:?} offset={:?}",
                        e.seq().unwrap_or(u64::MAX),
                        e.probe_name(),
                        e.u64("variant"),
                        e.u64("seg_idx"),
                        e.u64("segment_index"),
                        e.u64("init_len"),
                        e.u64("offset"),
                    )
                })
                .collect();
            panic!(
                "T9 RED — `commit_segment(V_new={variant_to}, seg_idx=0, init_len > 0)` \
                 did not fire within 8s of ABR commit (apply.seq={apply_seq}). \
                 Scheduler не зафетчил V_new seg-0 init вовсе. Probe events:\n{}",
                dump.join("\n")
            )
        });
    let commit_seq = commit_event
        .seq()
        .expect("`commit_segment` event must carry `seq`");

    // Ассерт: между apply.seq и commit.seq не срабатывает
    // metadata_range_for_segment(segment_index=0) — это сигнал, что
    // fallback B/C из format_change_segment_range отдал чужой range.
    let metadata_calls_in_window: Vec<String> = recorder
        .snapshot()
        .into_iter()
        .filter(|e| {
            e.probe_name() == Some("metadata_range_for_segment")
                && e.seq().is_some_and(|s| s > apply_seq && s < commit_seq)
                && e.u64("segment_index") == Some(0)
        })
        .map(|e| {
            format!(
                "  seq={} segment_index={:?} offset={:?} caller_fn={:?}",
                e.seq().unwrap_or(u64::MAX),
                e.u64("segment_index"),
                e.u64("offset"),
                e.caller_fn(),
            )
        })
        .collect();

    assert!(
        metadata_calls_in_window.is_empty(),
        "T9 RED — `metadata_range_for_segment(segment_index=0)` сработал {n} раз \
         в окне (apply.seq={apply_seq}, commit.seq={commit_seq}). Это означает, что \
         fallback-цепочка в `HlsSource::format_change_segment_range` отдала \
         метадата-range для seg-0 до того, как V_new seg-0 init был зацементирован. \
         AGENTS.md запрещает такие fallback-цепочки. События:\n{events}",
        n = metadata_calls_in_window.len(),
        events = metadata_calls_in_window.join("\n"),
    );
}
