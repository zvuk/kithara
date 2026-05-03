//! T6 — init-range outcome must always be `OkSeg0` for synthetic
//! wave fixtures (axiom A5, init-range subset).
//!
//! `record_init_range_for_variant_outcome` and
//! `record_segment_layout_init_range_call` USDT probes carry one of
//! seven outcomes, encoded by
//! `kithara_hls::source::init_range_probe::InitRangeOutcome`:
//!
//! * 0 — `OkSeg0` — segment 0 was already committed and its range
//!   came directly from `StreamIndex::item_range`.
//! * 1 — `OkMetadataFallback` — the byte map for seg-0 was not yet
//!   committed; range came from playlist metadata.
//! * 2 — `OkSeg0ViaRange` — the init-bearing segment was not seg-0
//!   itself, range was synthesised from seg-0's committed range.
//! * 3..6 — `None*` — all the failure paths.
//!
//! For the contract suite all fixtures present every variant's seg-0
//! upfront with an explicit `init_url`, so the only correct outcome
//! is **`OkSeg0` (0)**. Any `OkMetadataFallback` / `OkSeg0ViaRange`
//! would mean the resolver fell back through fixture-metadata or the
//! late-init synthesis path; both are code smells (workarounds for
//! commit-ordering bugs that the contract tests must catch).

use kithara::{
    assets::StoreOptions,
    audio::{Audio, ChunkOutcome, PcmReader},
    decode::DecoderBackend,
    hls::{AbrMode, Hls},
    stream::Stream,
};
use kithara_platform::time::{Duration, Instant};
use kithara_test_utils::{TestServerHelper, TestTempDir, probe_capture, temp_dir};

use super::helpers::{
    Consts,
    params::{open_audio, wave_fixture_4_variants},
};

/// Drive the audio through enough chunks so the demuxer has at least
/// requested `init_segment_range()` once per variant we'll switch to.
async fn warmup_then_switch_through_all_variants(
    audio: &mut Audio<Stream<Hls>>,
    backend: DecoderBackend,
) {
    use kithara::audio::ReadOutcome;
    let _ = backend; // kept for future per-backend tuning

    let warmup_deadline = Instant::now() + Duration::from_secs(8);
    let mut buf = vec![0.0f32; 4096];
    let mut warmup_samples = 0u64;
    while Instant::now() < warmup_deadline && warmup_samples < 8000 {
        let _ = audio.preload();
        match audio.read(&mut buf) {
            Ok(ReadOutcome::Frames { count, .. }) => warmup_samples += count.get() as u64,
            Ok(ReadOutcome::Pending { .. }) => {
                kithara_platform::time::sleep(Duration::from_millis(5)).await;
            }
            Ok(ReadOutcome::Eof { .. }) => break,
            Err(e) => panic!("T6: decode error during warmup: {e}"),
        }
    }
    assert!(
        warmup_samples > 0,
        "T6: warmup failed to produce any samples"
    );

    // Walk through every variant in turn. Each variant flip should
    // trigger a fresh `Fmp4SegmentDemuxer::open` ⇒ a fresh
    // `init_segment_range()` call recorded by the
    // `record_segment_layout_init_range_call` probe.
    for variant in [
        Consts::VARIANT_AAC_LQ,
        Consts::VARIANT_AAC_MQ,
        Consts::VARIANT_AAC_HQ,
        Consts::VARIANT_FLAC,
    ] {
        audio
            .abr_handle()
            .expect("HLS Audio must expose an ABR handle")
            .set_mode(AbrMode::Manual(variant))
            .expect("set_mode");
        let deadline = Instant::now() + Duration::from_secs(6);
        let mut found_chunk_on_variant = false;
        while Instant::now() < deadline {
            let _ = audio.preload();
            match PcmReader::next_chunk(audio) {
                Ok(ChunkOutcome::Chunk(chunk)) => {
                    if chunk.meta.variant_index == Some(variant) {
                        found_chunk_on_variant = true;
                        break;
                    }
                }
                Ok(ChunkOutcome::Pending { .. }) => {
                    kithara_platform::time::sleep(Duration::from_millis(5)).await;
                }
                Ok(ChunkOutcome::Eof { .. }) => break,
                Err(e) => panic!("T6: decode error during variant walk: {e}"),
            }
        }
        assert!(
            found_chunk_on_variant,
            "T6: never received a chunk on variant {variant} after set_mode \
             — switch did not propagate end-to-end (probe data not collectible)"
        );
    }
}

#[kithara::test(
    tokio,
    native,
    serial,
    timeout(Duration::from_secs(90)),
    env(KITHARA_HANG_TIMEOUT_SECS = "5")
)]
#[case::sw(DecoderBackend::Symphonia)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::hw(DecoderBackend::Apple)
)]
async fn t6_init_range_outcome_is_ok_seg0(temp_dir: TestTempDir, #[case] backend: DecoderBackend) {
    let recorder = probe_capture::install();
    let server = TestServerHelper::new().await;
    let created = server
        .create_hls(wave_fixture_4_variants())
        .await
        .expect("create wave HLS fixture");
    let url = created.master_url();
    let store = StoreOptions::new(temp_dir.path());

    let mut audio = open_audio(
        &url,
        store,
        AbrMode::Manual(Consts::VARIANT_AAC_LQ),
        backend,
        3,
    )
    .await;

    warmup_then_switch_through_all_variants(&mut audio, backend).await;

    // Both probe sites — `record_init_range_for_variant_outcome`
    // (called from `format_change_segment_range`) and
    // `record_segment_layout_init_range_call` (called from the fmp4
    // segment demuxer) — must emit `outcome = 0 (OkSeg0)` on every
    // single firing. Anything else is a fallback path; on the contract
    // fixtures (which always present seg-0 upfront), no fallback is
    // legitimate.
    let init_range_for_variant: Vec<_> =
        recorder.events_with_probe("record_init_range_for_variant_outcome");
    assert!(
        !init_range_for_variant.is_empty(),
        "T6: zero `record_init_range_for_variant_outcome` events. \
         The probe never fired — either `format_change_segment_range` was \
         never invoked, or the variant walk did not actually trigger a \
         decoder rebuild. Backend = {backend:?}."
    );
    for (idx, evt) in init_range_for_variant.iter().enumerate() {
        let outcome = evt.u64("outcome").expect("probe must include outcome");
        let variant = evt.u64("variant").expect("probe must include variant");
        let committed = evt.u64("committed_count").unwrap_or(0);
        let seg_idx_with_init = evt.u64("seg_idx_with_init");
        assert_eq!(
            outcome, 0,
            "T6: record_init_range_for_variant_outcome event #{idx} returned \
             non-OkSeg0 outcome={outcome} for variant={variant} \
             (committed_count={committed}, seg_idx_with_init={seg_idx_with_init:?}). \
             The fixture always commits seg-0 upfront with an explicit init_url, \
             so the resolver should never need a fallback path. Backend = {backend:?}."
        );
    }

    let layout_calls: Vec<_> = recorder.events_with_probe("record_segment_layout_init_range_call");
    assert!(
        !layout_calls.is_empty(),
        "T6: zero `record_segment_layout_init_range_call` events. \
         `Fmp4SegmentDemuxer::open` should fire this probe on every \
         demuxer rebuild. Backend = {backend:?}."
    );
    for (idx, evt) in layout_calls.iter().enumerate() {
        let outcome = evt.u64("outcome").expect("probe must include outcome");
        let current_variant = evt
            .u64("current_variant")
            .expect("probe must include current_variant");
        let committed = evt.u64("committed_count").unwrap_or(0);
        let seg_idx_with_init = evt.u64("seg_idx_with_init");
        assert_eq!(
            outcome, 0,
            "T6: record_segment_layout_init_range_call event #{idx} returned \
             non-OkSeg0 outcome={outcome} for current_variant={current_variant} \
             (committed_count={committed}, seg_idx_with_init={seg_idx_with_init:?}). \
             Backend = {backend:?}."
        );
    }

    // The factory must NEVER complain about a missing init range on
    // the contract fixture. `error_kind = 0 (InitRangeMissing)` is
    // the production sentinel for the ABR mid-stream switch race.
    let factory_failures: Vec<_> = recorder
        .events_with_probe("record_create_from_media_info_failed")
        .into_iter()
        .filter(|e| e.u64("error_kind") == Some(0))
        .collect();
    assert!(
        factory_failures.is_empty(),
        "T6: factory returned `init_range_missing` {n} time(s) — the ABR \
         init-range contract failed end-to-end. Backend = {backend:?}.",
        n = factory_failures.len(),
    );
}
