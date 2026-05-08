//! T4 — seek freezes ABR (axiom A6).
//!
//! Between `audio.seek(t)` and the first PCM chunk emitted at the new
//! position, the ABR controller MUST NOT commit a variant change.
//! That includes deliberate sabotage from the network side (delay
//! rules that would normally trigger a downswitch via the throughput
//! estimator) — seek is a user gesture, and ABR-driven mid-seek
//! transitions multiply work and visibly degrade perceived
//! responsiveness.
//!
//! Probe contract: zero `record_abr_variant_committed` events fall
//! between `seek_at` (the wall-clock instant of the `seek` call) and
//! `resume_at` (the wall-clock instant the first post-seek chunk
//! lands).

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

    let mut audio = open_audio(&url, store, AbrMode::Auto(Some(start_variant)), backend, 3).await;

    let pre_seek_target = Duration::from_secs_f64(Consts::PRE_SWITCH_TARGET_SECS);
    let mut chunks: Vec<PcmChunk> = Vec::new();

    let warmup_deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < warmup_deadline {
        let _ = audio.preload();
        match PcmReader::next_chunk(&mut audio) {
            Ok(ChunkOutcome::Chunk(chunk)) => {
                let ts = chunk.meta.timestamp;
                chunks.push(chunk);
                if ts >= pre_seek_target {
                    break;
                }
            }
            Ok(ChunkOutcome::Pending { .. }) => {
                kithara_platform::time::sleep(Duration::from_millis(5)).await;
            }
            Ok(ChunkOutcome::Eof { .. }) => break,
            Err(e) => {
                panic!("T4 [start={start_variant} {backend:?}]: decode error during warmup: {e}")
            }
        }
    }
    assert!(
        chunks
            .last()
            .is_some_and(|c| c.meta.timestamp >= pre_seek_target),
        "T4 [start={start_variant} {backend:?}]: warmup never reached \
         pre_seek_target {pre_seek_target:?}"
    );

    let seek_target = Duration::from_secs_f64(
        Consts::SEGMENT_DURATION_SECS * (Consts::SEGMENTS_PER_VARIANT as f64) * 0.5,
    );
    let seek_at = Instant::now();
    audio
        .seek(seek_target)
        .expect("T4: seek must not fail on a healthy fixture");

    let resume_deadline = Instant::now() + Duration::from_secs(15);
    let mut resume_at: Option<Instant> = None;
    while Instant::now() < resume_deadline {
        let _ = audio.preload();
        match PcmReader::next_chunk(&mut audio) {
            Ok(ChunkOutcome::Chunk(chunk)) => {
                resume_at = Some(Instant::now());
                let _ = chunk;
                break;
            }
            Ok(ChunkOutcome::Pending { .. }) => {
                kithara_platform::time::sleep(Duration::from_millis(5)).await;
            }
            Ok(ChunkOutcome::Eof { .. }) => break,
            Err(e) => panic!("T4 [start={start_variant} {backend:?}]: decode error post-seek: {e}"),
        }
    }
    let resume_at = resume_at.unwrap_or_else(|| {
        panic!(
            "T4 [start={start_variant} {backend:?}]: no chunk delivered within 15 s \
             after seek({seek_target:?}) — playback never resumed"
        )
    });

    let commits_during_seek: Vec<_> = recorder
        .events_with_probe("record_abr_variant_committed")
        .into_iter()
        .filter(|e| e.at >= seek_at && e.at <= resume_at)
        .collect();
    assert!(
        commits_during_seek.is_empty(),
        "T4 [start={start_variant} {backend:?}] A6: ABR controller committed \
         {n} variant change(s) inside the seek window ({seek_at:?} .. {resume_at:?}): \
         {commits_during_seek:?}. ABR must be frozen between user-initiated seek \
         and the first post-seek chunk.",
        n = commits_during_seek.len(),
    );
}
