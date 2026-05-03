//! T2 — exact fetch count (axioms A2, A3, A4, A5).
//!
//! After a manual ABR switch, the scheduler MUST emit:
//!
//! * **A5** — exactly one `emit_fetch_cmd` event with
//!   `plan_need_init = true` for `variant_to`.
//! * **A3** — zero `emit_fetch_cmd` events with `plan_need_init = false`
//!   on `variant_to` whose `segment_index < playback_floor_idx`.
//! * **A4** — zero `emit_fetch_cmd` events with `plan_need_init = false`
//!   on `variant_to` whose `segment_index > playback_floor_idx + prefetch_count`.
//! * **A2** — every `(variant_to, segment_index)` is emitted at most once.
//! * No fetch on `variant_from` after the commit.
//!
//! `playback_floor_idx` is read from the timestamp of the last
//! pre-switch chunk; `prefetch_count` is the value the test passed
//! through `HlsConfig::with_download_batch_size`. No slack, no
//! "reasonable window" — exact integer counts.

use std::collections::BTreeMap;

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
    assertions::assert_exact_count,
    counters::{FetchEmit, fetch_emits_for, first_abr_commit},
    params::{NetworkProfile, open_audio, wave_fixture_4_variants},
};

#[expect(clippy::too_many_arguments, reason = "test parameter context")]
async fn run_case(
    temp_dir: TestTempDir,
    variant_from: usize,
    variant_to: usize,
    backend: DecoderBackend,
    prefetch_count: usize,
    network: NetworkProfile,
) {
    let recorder = probe_capture::install();
    let server = TestServerHelper::new().await;
    let created = server
        .create_hls(wave_fixture_4_variants().delay_rules(network.delay_rules()))
        .await
        .expect("create wave HLS fixture");
    let url = created.master_url();
    let store = StoreOptions::new(temp_dir.path());

    let mut audio = open_audio(
        &url,
        store,
        AbrMode::Manual(variant_from),
        backend,
        prefetch_count,
    )
    .await;

    let pre_switch_target = Duration::from_secs_f64(Consts::PRE_SWITCH_TARGET_SECS);
    let mut chunks: Vec<PcmChunk> = Vec::new();
    let mut switched = false;
    let mut first_to_idx: Option<usize> = None;
    let mut switch_at: Option<Instant> = None;
    let mut last_pre_chunk_ts: Duration = Duration::ZERO;

    let deadline = Instant::now() + Duration::from_secs(35);
    while Instant::now() < deadline {
        let _ = audio.preload();
        match PcmReader::next_chunk(&mut audio) {
            Ok(ChunkOutcome::Chunk(chunk)) => {
                let v = chunk.meta.variant_index;
                let ts = chunk.meta.timestamp;
                if v == Some(variant_from) {
                    last_pre_chunk_ts = ts;
                }
                chunks.push(chunk);
                if !switched && v == Some(variant_from) && ts >= pre_switch_target {
                    audio
                        .abr_handle()
                        .expect("HLS Audio must expose an ABR handle")
                        .set_mode(AbrMode::Manual(variant_to))
                        .expect("set_mode");
                    switched = true;
                    switch_at = Some(Instant::now());
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
            Err(e) => panic!(
                "T2 [{variant_from}->{variant_to} {backend:?} pf={prefetch_count} \
                 net={net}]: decode error: {e}",
                net = network.label(),
            ),
        }
    }

    let label = format!(
        "{variant_from}->{variant_to} {backend:?} pf={prefetch_count} net={net}",
        net = network.label(),
    );
    assert!(
        switched,
        "T2 [{label}]: warmup never reached pre_switch_target {pre_switch_target:?}"
    );

    // Anchor for fetch-emit filtering: the moment the ABR controller
    // actually committed the switch (preferred over `set_mode` wall
    // time, which can lead the commit by a few ticks).
    let commit_at = first_abr_commit(&recorder, variant_from, variant_to).unwrap_or_else(|| {
        switch_at.unwrap_or_else(|| {
            panic!(
                "T2 [{label}]: neither record_abr_variant_committed probe nor \
                     local switch wall-clock available — switch never committed"
            )
        })
    });

    let playback_floor_idx =
        (last_pre_chunk_ts.as_secs_f64() / Consts::SEGMENT_DURATION_SECS) as usize;

    let emits_to: Vec<FetchEmit> = fetch_emits_for(&recorder, variant_to, commit_at);

    // ---------- A5: exactly one init fetch on variant_to ----------
    let init_emits: Vec<&FetchEmit> = emits_to.iter().filter(|e| e.plan_need_init).collect();
    assert_exact_count(
        init_emits.len(),
        1,
        &format!("T2 [{label}] A5: init fetches on variant_to"),
    );

    // ---------- A3: zero back-fetches ----------
    let back_fetches: Vec<&FetchEmit> = emits_to
        .iter()
        .filter(|e| !e.plan_need_init && e.segment_index < playback_floor_idx)
        .collect();
    assert!(
        back_fetches.is_empty(),
        "T2 [{label}] A3: scheduler emitted {n} back-fetch(es) on variant_to in \
         segment_index < playback_floor ({playback_floor_idx}): {back_fetches:?}",
        n = back_fetches.len(),
    );

    // ---------- A4: bounded forward prefetch ----------
    let upper = playback_floor_idx + prefetch_count;
    let overreach: Vec<&FetchEmit> = emits_to
        .iter()
        .filter(|e| !e.plan_need_init && e.segment_index > upper)
        .collect();
    assert!(
        overreach.is_empty(),
        "T2 [{label}] A4: scheduler emitted {n} fetch(es) on variant_to past the \
         configured prefetch window (segment_index > {upper} = floor {playback_floor_idx} + \
         pf {prefetch_count}): {overreach:?}",
        n = overreach.len(),
    );

    // ---------- A2: idempotent emits per (variant_to, segment_index) ----------
    let mut seg_counts: BTreeMap<usize, usize> = BTreeMap::new();
    for emit in emits_to.iter().filter(|e| !e.plan_need_init) {
        *seg_counts.entry(emit.segment_index).or_insert(0) += 1;
    }
    let duplicates: Vec<(usize, usize)> = seg_counts
        .iter()
        .filter(|(_, count)| **count > 1)
        .map(|(seg, count)| (*seg, *count))
        .collect();
    assert!(
        duplicates.is_empty(),
        "T2 [{label}] A2: scheduler emitted duplicate fetches on variant_to: {duplicates:?}"
    );

    // ---------- variant_from must be quiet after commit ----------
    let from_emits = fetch_emits_for(&recorder, variant_from, commit_at);
    assert!(
        from_emits.is_empty(),
        "T2 [{label}]: scheduler kept fetching variant_from ({variant_from}) after \
         the switch commit: {from_emits:?}"
    );
}

#[kithara::test(
    tokio,
    native,
    serial,
    timeout(Duration::from_secs(60)),
    env(KITHARA_HANG_TIMEOUT_SECS = "3")
)]
#[case::lq_to_hq_pf1(
    Consts::VARIANT_AAC_LQ,
    Consts::VARIANT_AAC_HQ,
    DecoderBackend::Symphonia,
    1,
    NetworkProfile::Instant
)]
#[case::lq_to_hq_pf3(
    Consts::VARIANT_AAC_LQ,
    Consts::VARIANT_AAC_HQ,
    DecoderBackend::Symphonia,
    3,
    NetworkProfile::Instant
)]
#[case::lq_to_hq_pf8(
    Consts::VARIANT_AAC_LQ,
    Consts::VARIANT_AAC_HQ,
    DecoderBackend::Symphonia,
    8,
    NetworkProfile::Instant
)]
#[case::hq_to_lq_pf3(
    Consts::VARIANT_AAC_HQ,
    Consts::VARIANT_AAC_LQ,
    DecoderBackend::Symphonia,
    3,
    NetworkProfile::Instant
)]
#[case::hq_to_flac_pf3(
    Consts::VARIANT_AAC_HQ,
    Consts::VARIANT_FLAC,
    DecoderBackend::Symphonia,
    3,
    NetworkProfile::Instant
)]
#[case::flac_to_lq_pf3(
    Consts::VARIANT_FLAC,
    Consts::VARIANT_AAC_LQ,
    DecoderBackend::Symphonia,
    3,
    NetworkProfile::Instant
)]
#[case::lq_to_hq_pf3_slow(
    Consts::VARIANT_AAC_LQ, Consts::VARIANT_AAC_HQ, DecoderBackend::Symphonia, 3,
    NetworkProfile::Slow { target_variant: Consts::VARIANT_AAC_LQ }
)]
#[case::lq_to_hq_pf3_flaky(
    Consts::VARIANT_AAC_LQ,
    Consts::VARIANT_AAC_HQ,
    DecoderBackend::Symphonia,
    3,
    NetworkProfile::Flaky
)]
async fn t2_exact_fetch_count(
    temp_dir: TestTempDir,
    #[case] variant_from: usize,
    #[case] variant_to: usize,
    #[case] backend: DecoderBackend,
    #[case] prefetch_count: usize,
    #[case] network: NetworkProfile,
) {
    run_case(
        temp_dir,
        variant_from,
        variant_to,
        backend,
        prefetch_count,
        network,
    )
    .await;
}
