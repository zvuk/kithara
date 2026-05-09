//! T4 — seek freezes ABR (axiom A6).
//!
//! Между `audio.seek(t)` и первой пробой post-seek chunk'а ABR controller
//! не должен коммитить смену варианта. Включая network slow rules,
//! которые в обычной ситуации триггерят downswitch — seek это user
//! gesture, ABR-driven mid-seek transitions ухудшают responsiveness.
//!
//! Probe contract: ноль `record_abr_variant_committed` (=`apply`) событий
//! между `seek_at` и `resume_at` (первый build_chunk seq после seek_at).

use kithara::{assets::StoreOptions, decode::DecoderBackend, hls::AbrMode};
use kithara_platform::time::{Duration, Instant};
use kithara_test_utils::{TestServerHelper, TestTempDir, probe_capture, temp_dir};

use super::helpers::{
    Consts,
    params::{NetworkProfile, open_audio, wave_fixture_4_variants},
};

#[kithara::test(
    tokio,
    native,
    serial,
    timeout(Duration::from_secs(45)),
    env(KITHARA_HANG_TIMEOUT_SECS = "3")
)]
#[case::auto_starts_on_lq_sw(Consts::VARIANT_AAC_LQ, DecoderBackend::Symphonia)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::auto_starts_on_lq_hw(Consts::VARIANT_AAC_LQ, DecoderBackend::Apple)
)]
#[case::auto_starts_on_hq_sw(Consts::VARIANT_AAC_HQ, DecoderBackend::Symphonia)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::auto_starts_on_hq_hw(Consts::VARIANT_AAC_HQ, DecoderBackend::Apple)
)]
async fn t4_seek_freezes_abr(
    temp_dir: TestTempDir,
    #[case] start_variant: usize,
    #[case] backend: DecoderBackend,
) {
    let recorder = probe_capture::install();
    let server = TestServerHelper::new().await;
    let created = server
        .create_hls(
            wave_fixture_4_variants().delay_rules(
                NetworkProfile::Slow {
                    target_variant: start_variant,
                }
                .delay_rules(),
            ),
        )
        .await
        .expect("create wave HLS fixture");
    let url = created.master_url();
    let store = StoreOptions::new(temp_dir.path());

    let label = format!("start={start_variant} {backend:?}");
    let mut audio = open_audio(&url, store, AbrMode::Auto(Some(start_variant)), backend, 3).await;

    // Warmup — ждём пока декодер выпустит chunk с timestamp >= PRE_SEEK_TARGET.
    let pre_seek_target_us = (Consts::PRE_SWITCH_TARGET_SECS * 1_000_000.0) as u64;
    recorder
        .wait_for_probe(
            |e| {
                e.probe_name() == Some("build_chunk")
                    && e.u64("timestamp")
                        .is_some_and(|ts| ts >= pre_seek_target_us)
            },
            Duration::from_secs(15),
        )
        .unwrap_or_else(|| {
            panic!("T4 [{label}]: warmup `build_chunk` >= PRE_SEEK_TARGET not seen in 15s")
        });

    let seek_target = Duration::from_secs_f64(
        Consts::SEGMENT_DURATION_SECS * (Consts::SEGMENTS_PER_VARIANT as f64) * 0.5,
    );
    let seek_at = Instant::now();
    audio
        .seek(seek_target)
        .expect("T4: seek must not fail on a healthy fixture");

    // Ждём первый build_chunk после seek_at — это resume_at.
    let resume_event = recorder
        .wait_for_probe(
            |e| e.probe_name() == Some("build_chunk") && e.at >= seek_at,
            Duration::from_secs(15),
        )
        .unwrap_or_else(|| {
            panic!(
                "T4 [{label}]: no build_chunk delivered within 15s after seek({seek_target:?}) — \
                 playback never resumed"
            )
        });
    let resume_at = resume_event.at;

    let commits_during_seek: Vec<_> = recorder
        .events_with_probe("apply")
        .into_iter()
        .filter(|e| e.at >= seek_at && e.at <= resume_at)
        .collect();
    assert!(
        commits_during_seek.is_empty(),
        "T4 [{label}] A6: ABR controller committed {n} variant change(s) inside \
         the seek window ({seek_at:?} .. {resume_at:?}): {commits_during_seek:?}. \
         ABR must be frozen between user-initiated seek and the first post-seek chunk.",
        n = commits_during_seek.len(),
    );
}
