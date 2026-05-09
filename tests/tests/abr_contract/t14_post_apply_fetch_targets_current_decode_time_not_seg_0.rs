//! T14 — после `set_mode(V_new)` scheduler фетчит V_new по текущему
//! `decode_time`, а не seg-0 (контракт C-A5).
//!
//! Полностью probe-driven: ни одного polling-цикла. Pre-switch warmup —
//! через `wait_for_probe(build_chunk, timestamp >= 4s)`. Каждый шаг ABR
//! цепочки (`on_mode_changed → tick → apply`) — отдельная проба с
//! tight 2-секундным бюджетом и точным сообщением, который link сломан.

use kithara::{
    assets::StoreOptions,
    audio::{ChunkOutcome, PcmReader},
    decode::DecoderBackend,
    hls::AbrMode,
};
use kithara_platform::time::{Duration, Instant};
use kithara_test_utils::{TestServerHelper, TestTempDir, probe_capture, temp_dir};
use tokio::time::sleep;

use super::helpers::{
    Consts,
    diagnostics::format_probe_dump,
    params::{open_audio, wave_fixture_4_variants},
    probe_contracts::assert_post_apply_fetch_chain,
};

fn mode_variant(mode: AbrMode) -> usize {
    match mode {
        AbrMode::Manual(v) | AbrMode::Auto(Some(v)) => v,
        AbrMode::Auto(None) => 0,
    }
}

#[kithara::test(
    tokio,
    native,
    serial,
    timeout(Duration::from_secs(20)),
    env(KITHARA_HANG_TIMEOUT_SECS = "5")
)]
#[case::manual(
    AbrMode::Manual(Consts::VARIANT_AAC_LQ),
    AbrMode::Manual(Consts::VARIANT_AAC_HQ),
    Consts::VARIANT_AAC_HQ
)]
#[case::noop_manual(
    AbrMode::Manual(Consts::VARIANT_AAC_LQ),
    AbrMode::Manual(Consts::VARIANT_AAC_LQ),
    Consts::VARIANT_AAC_LQ
)]
async fn t14_post_apply_fetch_targets_current_decode_time_not_seg_0(
    temp_dir: TestTempDir,
    #[case] initial_mode: AbrMode,
    #[case] switch_mode: AbrMode,
    #[case] variant_to: usize,
) {
    let variant_from = mode_variant(initial_mode);
    let is_noop = variant_from == variant_to;
    let recorder = probe_capture::install();
    let server = TestServerHelper::new().await;
    let created = server
        .create_hls(wave_fixture_4_variants())
        .await
        .expect("create wave HLS fixture");
    let url = created.master_url();
    let store = StoreOptions::new(temp_dir.path());

    let mut audio = open_audio(&url, store, initial_mode, DecoderBackend::Symphonia, 3).await;

    // Active drain до тех пор, пока decoder не дойдёт до PRE_SWITCH_TARGET.
    // Аналог `wait_for_probe(build_chunk >= 4s)`, но через PCM consumption
    // (`audio.read_pcm().await`) — это держит peer.poll_next активным через
    // `reader_advanced` notify, в отличие от пассивного probe-watching'а.
    let pre_switch_target_us = (Consts::PRE_SWITCH_TARGET_SECS * 1_000_000.0) as u64;
    let warmup_started = Instant::now();
    let warmup_budget = Duration::from_secs(15);
    let mut last_decode_time_us: u64 = 0;
    while warmup_started.elapsed() < warmup_budget {
        match PcmReader::next_chunk(&mut audio) {
            Ok(ChunkOutcome::Chunk(chunk)) => {
                let ts_us = (chunk.meta.timestamp.as_micros())
                    .try_into()
                    .unwrap_or(u64::MAX);
                last_decode_time_us = ts_us;
                if ts_us >= pre_switch_target_us {
                    break;
                }
            }
            Ok(ChunkOutcome::Pending { .. }) => {
                sleep(Duration::from_millis(5)).await;
            }
            Ok(ChunkOutcome::Eof { .. }) => {
                panic!("T14 setup — natural EOF before PRE_SWITCH_TARGET");
            }
            Err(e) => panic!("T14 setup — decode error during warmup: {e}"),
        }
    }
    if last_decode_time_us < pre_switch_target_us {
        let dump = format_probe_dump(&recorder, 80);
        panic!(
            "T14 setup — drain loop budget {budget_s}s elapsed before \
             decoder reached PRE_SWITCH_TARGET={pre_switch_target_us}μs \
             (last seen ts={last_decode_time_us}μs).\n\
             warmup chain dump (operator: смотри какая проба остановилась — \
             emit_fetch_cmd, finish_request, commit_segment, build_chunk):\n{dump}",
            budget_s = warmup_budget.as_secs(),
        );
    }

    // Триггер switch.
    audio
        .abr_handle()
        .expect("HLS Audio must expose an ABR handle")
        .set_mode(switch_mode)
        .expect("set_mode");

    let switch_mode_packed: usize = switch_mode.into();

    // ABR chain: каждый link — отдельная проба, отдельный panic.
    let on_mode_changed = recorder
        .wait_for_probe_async(
            |e| {
                e.probe_name() == Some("on_mode_changed")
                    && e.u64("mode") == Some(switch_mode_packed as u64)
            },
            Duration::from_secs(2),
        )
        .await
        .expect(
            "T14 RED — `AbrController::on_mode_changed` did not fire with \
             target mode within 2s of `AbrHandle::set_mode`.",
        );

    let tick_after_mode = recorder
        .wait_for_probe_async(
            |e| {
                e.probe_name() == Some("tick")
                    && e.seq()
                        .zip(on_mode_changed.seq())
                        .is_some_and(|(s, m)| s > m)
                    && e.u64("peer_id") == on_mode_changed.u64("peer_id")
            },
            Duration::from_secs(2),
        )
        .await
        .expect(
            "T14 RED — `on_mode_changed` fired but `Controller::tick` did \
             not run for the same peer within 2s.",
        );

    // Для noop switch (variant_from == variant_to) `state.apply` не вызывается
    // намеренно: ABR controller рано выходит при `decision.did_change == false`.
    // Берём baseline на peer-thread'е: thread_seq на котором сидит scheduler
    // (peer.poll_next либо tick — оба бьют на peer-thread'е).
    let (peer_thread_id, baseline_thread_seq) = if is_noop {
        let tid = tick_after_mode
            .thread_id()
            .expect("`tick` event must carry `thread_id`");
        let ts = tick_after_mode
            .thread_seq()
            .expect("`tick` event must carry `thread_seq`");
        (tid, ts)
    } else {
        let apply_event = recorder
            .wait_for_probe_async(
                |e| {
                    e.probe_name() == Some("apply")
                        && e.u64("d") == Some(variant_to as u64)
                        && e.seq()
                            .zip(tick_after_mode.seq())
                            .is_some_and(|(s, t)| s > t)
                },
                Duration::from_secs(2),
            )
            .await
            .expect(
                "T14 RED — `tick` ran but `AbrState::apply` was not called \
                 for the target variant within 2s.",
            );
        let tid = apply_event
            .thread_id()
            .expect("`apply` event must carry `thread_id`");
        let ts = apply_event
            .thread_seq()
            .expect("`apply` event must carry `thread_seq`");
        (tid, ts)
    };

    // Active drain post-apply: continue PCM consumption так чтобы peer
    // получал reader_advanced wake-ups и мог обработать variant change.
    // Без drain loop'а system замораживается (decoder исчерпывает буфер,
    // reader stuck, peer Pending без триггера). Останавливаем drain
    // как только видим post-apply emit для V_new ИЛИ budget elapsed.
    let post_apply_budget = Duration::from_secs(8);
    let post_apply_started = Instant::now();
    while post_apply_started.elapsed() < post_apply_budget {
        match PcmReader::next_chunk(&mut audio) {
            Ok(ChunkOutcome::Chunk(_) | ChunkOutcome::Pending { .. }) => {
                let post_apply_emit = recorder.snapshot().into_iter().any(|e| {
                    e.thread_id() == Some(peer_thread_id)
                        && e.thread_seq().is_some_and(|s| s > baseline_thread_seq)
                        && e.probe_name() == Some("emit_fetch_cmd")
                        && e.u64("variant") == Some(variant_to as u64)
                        && e.u64("plan_need_init") == Some(1)
                });
                if post_apply_emit {
                    break;
                }
                sleep(Duration::from_millis(5)).await;
            }
            Ok(ChunkOutcome::Eof { .. }) => break,
            Err(e) => panic!("T14 — decode error during post-apply drain: {e}"),
        }
    }

    // Per-thread sequence contract: post-apply scheduler chain on
    // peer-thread должен пройти `apply_variant_readiness(V_new) →
    // build_batch(V_new) → emit_fetch_cmd(V_new, plan_need_init=1)`.
    // На failure error message называет какой link сломан и что было
    // вместо ожидаемого.
    // forward_only_floor = decode_seg the player was on when set_mode
    // fired. After a switch the decoder has all of [0, floor) already
    // produced; scheduler must not re-fetch those on V_new until the
    // user does a seek (which would fire `seek_epoch_reset`).
    let decode_seg_at_apply =
        ((last_decode_time_us as f64) / 1_000_000.0 / Consts::SEGMENT_DURATION_SECS) as usize;
    let fetch_event = assert_post_apply_fetch_chain(
        &recorder,
        peer_thread_id,
        baseline_thread_seq,
        variant_from,
        variant_to,
        !is_noop,
        decode_seg_at_apply,
    )
    .unwrap_or_else(|err| panic!("T14 RED — post-apply chain broken:\n{err}"));

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

    // C-A5 forward-only: первый post-apply fetch для V_new должен иметь
    // `segment_index >= decode_seg` И НЕ быть init range (seg=0). Это значит
    // scheduler не качает уже-played segments (forward-only) и не trogает
    // V_new init range, который satisfied locally из pre-loaded cache.
    //
    // Верхняя граница drift зависит от reader prefetch lookahead: reader
    // буферизует ~prefetch_count segments вперёд decoder'а до switch'а,
    // так после switch'а scheduler legitimately начинает с reader_floor =
    // decode_seg + lookahead. Ставим верхнюю границу `prefetch_count + 2`
    // (запас на reader's pre-fetch buffer + cursor advance).
    //
    // В noop case (variant_from == variant_to) switch'а нет, axiom не
    // применяется как strict — scheduler продолжает обычный forward
    // prefetch.
    let segment_index = fetch_event
        .u64("segment_index")
        .expect("emit_fetch_cmd carries segment_index");
    let last_decode_secs = (last_decode_time_us as f64) / 1_000_000.0;
    let expected_segment = (last_decode_secs / Consts::SEGMENT_DURATION_SECS) as u64;
    let signed_drift =
        i64::try_from(segment_index).unwrap_or(0) - i64::try_from(expected_segment).unwrap_or(0);
    if is_noop {
        // Forward-only в noop: drift >= 0, без верхней границы.
        if signed_drift < 0 {
            let dump = format_probe_dump(&recorder, 60);
            panic!(
                "T14 RED (noop) — fetch для V={variant_to} targeted \
                 seg={segment_index}, decoder playback {last_decode_secs:.2}s \
                 → decode_seg={expected_segment}, signed_drift={signed_drift}. \
                 REGRESSION: scheduler качает seg ПОЗАДИ decoder playback.\n{dump}",
            );
        }
    } else {
        // C-A5 для switch: forward-only + не качаем seg=0 (init).
        let max_drift: i64 = 5; // prefetch_count(=3) + reader_lookahead(~2)
        if signed_drift < 0 || segment_index == 0 {
            let dump = format_probe_dump(&recorder, 60);
            panic!(
                "T14 RED — first post-apply fetch для V_new ({variant_to}) \
                 targeted seg={segment_index}, decoder playback при apply = \
                 {last_decode_secs:.2}s → decode_seg={expected_segment}. \
                 C-A5 violation: {direction}\n{dump}",
                direction = if signed_drift < 0 {
                    "scheduler качает seg ПОЗАДИ decoder playback (forward-only invariant)"
                } else {
                    "scheduler качает V_new init range (seg=0) — должен быть satisfied \
                     locally из pre-loaded init cache"
                },
            );
        }
        if signed_drift > max_drift {
            let dump = format_probe_dump(&recorder, 60);
            panic!(
                "T14 RED — first post-apply fetch для V_new ({variant_to}) \
                 targeted seg={segment_index}, decoder playback {last_decode_secs:.2}s \
                 → decode_seg={expected_segment}, signed_drift={signed_drift} \
                 за пределы [0, {max_drift}]. \
                 Scheduler забегает слишком далеко вперёд decoder'а — decoder \
                 будет ждать пока scheduler докачает остаток.\n{dump}",
            );
        }
    }
}
