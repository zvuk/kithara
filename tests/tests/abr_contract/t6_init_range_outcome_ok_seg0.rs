//! T6 — init-range outcome must always be `OkSeg0` for synthetic
//! wave fixtures (axiom A5, init-range subset).
//!
//! `HlsSource::init_segment_range_for_variant` returns
//! `InitRangeResolution { outcome, variant, committed_count,
//! seg_idx_with_init, range }`. The `#[kithara::probe(probe_return)]`
//! attribute on that function makes the resolution itself emit a
//! `init_segment_range_for_variant` probe.
//!
//! Outcome wire encoding (from `InitRangeOutcome::into_probe_arg`):
//!
//! * 0 — `OkSeg0`
//! * 1 — `OkMetadataFallback`
//! * 2 — `OkSeg0ViaRange`
//! * 3..6 — `None*`
//!
//! Для контрактного fixture seg-0 каждого варианта закоммичен upfront
//! с explicit `init_url`, так что единственный корректный outcome —
//! **`OkSeg0` (0)**.
//!
//! Probe-driven прогресс. Warmup и переходы между вариантами
//! отслеживаются через `build_chunk(timestamp)` и `apply_format_change`.

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

    let audio = open_audio(
        &url,
        store,
        AbrMode::Manual(Consts::VARIANT_AAC_LQ),
        backend,
        3,
    )
    .await;

    // Warmup — ждём пока декодер выпустит chunk с timestamp >= 1s.
    recorder
        .wait_for_probe(
            |e| {
                e.probe_name() == Some("build_chunk")
                    && e.u64("timestamp").is_some_and(|ts| ts >= 1_000_000)
            },
            Duration::from_secs(15),
        )
        .unwrap_or_else(|| panic!("T6: warmup `build_chunk` >= 1s not seen in 15s"));

    // Переключаемся через все варианты, для каждого ждём
    // `apply_format_change` — сигнал, что декодер пересоздан.
    for variant in [
        Consts::VARIANT_AAC_LQ,
        Consts::VARIANT_AAC_MQ,
        Consts::VARIANT_AAC_HQ,
        Consts::VARIANT_FLAC,
    ] {
        // Снимем seq до set_mode чтобы фильтровать «новый» recreate.
        let pre_seq = recorder
            .snapshot()
            .last()
            .and_then(|e| e.seq())
            .unwrap_or(0);

        audio
            .abr_handle()
            .expect("HLS Audio must expose an ABR handle")
            .set_mode(AbrMode::Manual(variant))
            .expect("set_mode");

        recorder
            .wait_for_probe(
                |e| {
                    e.probe_name() == Some("apply_format_change")
                        && e.seq().is_some_and(|s| s > pre_seq)
                },
                Duration::from_secs(8),
            )
            .unwrap_or_else(|| {
                panic!(
                    "T6: apply_format_change after set_mode(Manual({variant})) \
                     не сработал в 8s — switch не дошёл до recreate. Backend = {backend:?}."
                )
            });
    }

    let init_range_for_variant: Vec<_> =
        recorder.events_with_probe("init_segment_range_for_variant");
    assert!(
        !init_range_for_variant.is_empty(),
        "T6: zero `init_segment_range_for_variant` events. The probe \
         never fired — either the audio pipeline never asked the source to \
         resolve init-range, or `HlsSource::init_segment_range_for_variant` \
         was skipped during the variant walk. Backend = {backend:?}."
    );
    for (idx, evt) in init_range_for_variant.iter().enumerate() {
        let outcome = evt.u64("outcome").expect("probe must include outcome");
        let variant = evt.u64("variant").expect("probe must include variant");
        let committed = evt.u64("committed_count").unwrap_or(0);
        let seg_idx_with_init = evt.u64("seg_idx_with_init");
        assert_eq!(
            outcome, 0,
            "T6: init_segment_range_for_variant event #{idx} returned \
             non-OkSeg0 outcome={outcome} for variant={variant} \
             (committed_count={committed}, seg_idx_with_init={seg_idx_with_init:?}). \
             Fixture commits seg-0 upfront with explicit init_url, \
             resolver should never need a fallback path. Backend = {backend:?}."
        );
    }
}
