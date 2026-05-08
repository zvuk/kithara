//! T5 — switch acknowledgment latency (axiom A7).
//!
//! Wall-clock time from `set_mode(Manual(variant_to))` to the first
//! `PcmChunk` carrying `variant_index = Some(variant_to)` must not
//! exceed one segment duration of the test fixture. A bounded latency
//! is the only way to guarantee that user-initiated quality switches
//! feel responsive — anything beyond `segment_duration` means the
//! user has heard at least one extra segment of the OLD variant they
//! explicitly asked the player to leave.
//!
//! Latency is measured at the test-process boundary (between the
//! `set_mode` call and the first matching chunk return), not via
//! probes — the user-facing contract IS the wall-clock latency at the
//! call site.
//!
//! The contract is asserted on instant-network only. Slow / flaky
//! profiles legitimately stretch latency by network delay; T5 does
//! not weaken its bound to absorb that — instead, those profiles are
//! out of scope here and covered by T2 + T1 (which check exact
//! download counts and audio continuity regardless of latency).

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
    params::{open_audio, wave_fixture_4_variants},
};

#[kithara::test(
    tokio,
    native,
    serial,
    timeout(Duration::from_secs(45)),
    env(KITHARA_HANG_TIMEOUT_SECS = "3")
)]
#[case::lq_to_hq_sw(
    Consts::VARIANT_AAC_LQ,
    Consts::VARIANT_AAC_HQ,
    DecoderBackend::Symphonia
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::lq_to_hq_hw(Consts::VARIANT_AAC_LQ, Consts::VARIANT_AAC_HQ, DecoderBackend::Apple)
)]
#[case::hq_to_lq_sw(
    Consts::VARIANT_AAC_HQ,
    Consts::VARIANT_AAC_LQ,
    DecoderBackend::Symphonia
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::hq_to_lq_hw(Consts::VARIANT_AAC_HQ, Consts::VARIANT_AAC_LQ, DecoderBackend::Apple)
)]
#[case::hq_to_flac_sw(
    Consts::VARIANT_AAC_HQ,
    Consts::VARIANT_FLAC,
    DecoderBackend::Symphonia
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::hq_to_flac_hw(Consts::VARIANT_AAC_HQ, Consts::VARIANT_FLAC, DecoderBackend::Apple)
)]
#[case::flac_to_lq_sw(
    Consts::VARIANT_FLAC,
    Consts::VARIANT_AAC_LQ,
    DecoderBackend::Symphonia
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::flac_to_lq_hw(Consts::VARIANT_FLAC, Consts::VARIANT_AAC_LQ, DecoderBackend::Apple)
)]
async fn t5_switch_latency_bounded(
    temp_dir: TestTempDir,
    #[case] variant_from: usize,
    #[case] variant_to: usize,
    #[case] backend: DecoderBackend,
) {
    let _recorder = probe_capture::install();
    let server = TestServerHelper::new().await;
    let created = server
        .create_hls(wave_fixture_4_variants())
        .await
        .expect("create wave HLS fixture");
    let url = created.master_url();
    let store = StoreOptions::new(temp_dir.path());

    let mut audio = open_audio(&url, store, AbrMode::Manual(variant_from), backend, 3).await;

    let pre_switch_target = Duration::from_secs_f64(Consts::PRE_SWITCH_TARGET_SECS);
    let segment_duration = Duration::from_secs_f64(Consts::SEGMENT_DURATION_SECS);

    let mut switched_at: Option<Instant> = None;
    let mut first_to_at: Option<Instant> = None;
    let mut chunks: Vec<PcmChunk> = Vec::new();

    let deadline = Instant::now() + Duration::from_secs(35);
    while Instant::now() < deadline {
        let _ = audio.preload();
        match PcmReader::next_chunk(&mut audio) {
            Ok(ChunkOutcome::Chunk(chunk)) => {
                let v = chunk.meta.variant_index;
                let ts = chunk.meta.timestamp;
                chunks.push(chunk);
                if switched_at.is_none() && v == Some(variant_from) && ts >= pre_switch_target {
                    audio
                        .abr_handle()
                        .expect("HLS Audio must expose an ABR handle")
                        .set_mode(AbrMode::Manual(variant_to))
                        .expect("set_mode");
                    switched_at = Some(Instant::now());
                }
                if switched_at.is_some() && v == Some(variant_to) {
                    first_to_at = Some(Instant::now());
                    break;
                }
            }
            Ok(ChunkOutcome::Pending { .. }) => {
                kithara_platform::time::sleep(Duration::from_millis(5)).await;
            }
            Ok(ChunkOutcome::Eof { .. }) => break,
            Err(e) => panic!("T5 [{variant_from}->{variant_to} {backend:?}]: decode error: {e}"),
        }
    }

    let switched_at = switched_at.unwrap_or_else(|| {
        panic!(
            "T5 [{variant_from}->{variant_to} {backend:?}]: never reached \
             pre_switch_target {pre_switch_target:?}"
        )
    });
    let first_to_at = first_to_at.unwrap_or_else(|| {
        panic!(
            "T5 [{variant_from}->{variant_to} {backend:?}]: switch never propagated — \
             no chunk on variant_to within 35 s. Variants observed: {trace:?}",
            trace = chunks
                .iter()
                .map(|c| c.meta.variant_index)
                .collect::<Vec<_>>(),
        )
    });

    let latency = first_to_at.duration_since(switched_at);
    assert!(
        latency <= segment_duration,
        "T5 [{variant_from}->{variant_to} {backend:?}] A7: switch latency \
         {latency:?} exceeds segment duration {segment_duration:?}. The user \
         heard at least one extra segment of variant_from after asking to switch."
    );
}
