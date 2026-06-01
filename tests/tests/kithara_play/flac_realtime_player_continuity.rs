#![forbid(unsafe_code)]

use std::time::{Duration, Instant};

use kithara::{
    abr::AbrMode,
    assets::StoreOptions,
    play::{Resource, ResourceConfig},
    stream::dl::{Downloader, DownloaderConfig},
};
use kithara_decode::DecoderBackend;
use kithara_integration_tests::{
    HlsFixtureBuilder, TestServerHelper, TestTempDir, fixture_protocol::DelayRule,
    offline::OfflinePlayer,
};
use kithara_net::{HttpClient, NetOptions};
use kithara_platform::CancellationToken;
use kithara_stream::AudioCodec;
use tokio::time::sleep;
use tracing::{info, warn};

use crate::phase_continuity::common::{
    CHANNELS, FREQ_HZ, PhaseDrift, SAMPLE_RATE, SinePhaseSpec, scan_rendered_pcm,
};

const SEGMENT_DURATION_SECS: f64 = 2.0;
const SEGMENTS_PER_VARIANT: usize = 30;
const VARIANT_COUNT: usize = 3;
const TOP_VARIANT: usize = VARIANT_COUNT - 1;
const BLOCK_FRAMES: usize = 512;
/// Render this many seconds of sustained playback — long enough to cover the
/// "10–30 s after switching to FLAC" window where the production skip recurs.
const PLAY_SECS: f64 = 40.0;
/// In the switch scenario: play the AAC lower variant this long before
/// switching to the FLAC top variant.
const SWITCH_AT_SECS: f64 = 4.0;
/// Skip this much post-switch output before scanning, so the one-time
/// cross-codec recreate seam (a separate, already-pinned ~1024-sample warmup
/// drift) does not mask the periodic sustained-playback skip we are hunting.
const SWITCH_SETTLE_SECS: f64 = 2.5;
/// Scan window cadence: one phase check every 1/8 s of rendered output.
const SCAN_INTERVAL_FRAMES: u64 = SAMPLE_RATE as u64 / 8;

#[derive(Debug, Clone, Copy)]
enum Scenario {
    /// Start on the FLAC top variant and play sustained.
    SustainedFlac,
    /// Start on the AAC lower variant, switch to the FLAC top variant mid-play
    /// (the production "switch to highest quality" path), then play sustained.
    SwitchToFlac,
}

/// Build a sine HLS fixture: AAC ladder with a FLAC lossless top variant
/// (mirrors zvuk masters). The same 440 Hz full-scale sine is encoded into
/// every variant, phase-aligned across them.
fn build_fixture(delay_ms: Option<u64>) -> HlsFixtureBuilder {
    let mut b = HlsFixtureBuilder::new()
        .variant_count(VARIANT_COUNT)
        .segments_per_variant(SEGMENTS_PER_VARIANT)
        .segment_duration_secs(SEGMENT_DURATION_SECS)
        .packaged_audio_sine_aac_lc(SAMPLE_RATE, CHANNELS, FREQ_HZ)
        .override_variant_codec(TOP_VARIANT, AudioCodec::Flac);
    if let Some(ms) = delay_ms {
        b = b.delay_rules(vec![DelayRule {
            variant: None,
            segment_eq: None,
            segment_gte: Some(1),
            delay_ms: ms,
        }]);
    }
    b
}

/// Render `target_secs` of audio through the real player loop, paced so the
/// network/decoder have wall-clock time to deliver data (an unpaced pull
/// starves the decode worker and renders pure silence). Appends the
/// interleaved stereo capture to `out`. `out_rate` is the player's output
/// (host) sample rate, used to size the render in output frames.
async fn render_into(
    player: &mut OfflinePlayer,
    out: &mut Vec<f32>,
    target_secs: f64,
    out_rate: u32,
    wall_budget_ms: u64,
) {
    const BATCH: u32 = 8;
    const TICK_MS: u64 = 10;
    let target_blocks = (target_secs * f64::from(out_rate) / BLOCK_FRAMES as f64).ceil() as u32;
    let deadline = Instant::now() + Duration::from_millis(wall_budget_ms);
    let mut rendered = 0u32;
    while rendered < target_blocks {
        for _ in 0..BATCH {
            out.extend_from_slice(&player.render(BLOCK_FRAMES));
            rendered += 1;
        }
        if Instant::now() >= deadline {
            break;
        }
        sleep(Duration::from_millis(TICK_MS)).await;
    }
}

async fn run_case(
    scenario: Scenario,
    backend: DecoderBackend,
    delay_ms: Option<u64>,
    out_rate: u32,
) {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    kithara_integration_tests::apple_warmup::warm_if_apple(backend);

    let helper = TestServerHelper::new().await;
    let created = helper
        .create_hls(build_fixture(delay_ms))
        .await
        .expect("create sine HLS fixture");
    let master = created.master_url();

    let temp = TestTempDir::new();
    let store = StoreOptions::new(temp.path());
    let downloader = Downloader::new(
        DownloaderConfig::for_client(HttpClient::new(
            NetOptions::default(),
            CancellationToken::default(),
        ))
        .build(),
    );

    let initial_mode = match scenario {
        Scenario::SustainedFlac => AbrMode::manual(TOP_VARIANT),
        Scenario::SwitchToFlac => AbrMode::manual(0),
    };
    let cfg = ResourceConfig::for_src(master.as_str())
        .expect("valid master URL")
        .downloader(downloader)
        .name("t0".to_string())
        .store(store)
        .decoder_backend(backend)
        .initial_abr_mode(initial_mode)
        .build();

    let resource = Resource::new(cfg)
        .await
        .unwrap_or_else(|e| panic!("Resource::new failed: {e:?}"));
    let abr = resource.abr_handle();

    let mut player = OfflinePlayer::new(out_rate);
    player.load_and_fadein(resource, "t0");

    let chan = CHANNELS as usize;
    let wall_budget_ms = (PLAY_SECS * 1000.0 / 4.0) as u64 + delay_ms.unwrap_or(0) * 8 + 5_000;
    let mut pcm: Vec<f32> = Vec::new();
    let scan_from_frame: u64 = match scenario {
        Scenario::SustainedFlac => {
            render_into(&mut player, &mut pcm, PLAY_SECS, out_rate, wall_budget_ms).await;
            0
        }
        Scenario::SwitchToFlac => {
            render_into(
                &mut player,
                &mut pcm,
                SWITCH_AT_SECS,
                out_rate,
                wall_budget_ms,
            )
            .await;
            let switch_at_frame = (pcm.len() / chan) as u64;
            let handle = abr.as_ref().expect("HLS resource exposes an ABR handle");
            if let Err(e) = handle.set_mode(AbrMode::manual(TOP_VARIANT)) {
                warn!(?e, "switch to FLAC top failed");
            }
            render_into(
                &mut player,
                &mut pcm,
                PLAY_SECS - SWITCH_AT_SECS,
                out_rate,
                wall_budget_ms,
            )
            .await;
            switch_at_frame + (SWITCH_SETTLE_SECS * f64::from(out_rate)) as u64
        }
    };

    let frames = pcm.len() / chan;
    let played_secs = frames as f64 / f64::from(out_rate);
    info!(
        ?scenario,
        ?backend,
        delay_ms,
        out_rate,
        played_secs,
        position = player.position(),
        scan_from_frame,
        "render captured"
    );
    assert!(
        played_secs > PLAY_SECS * 0.5,
        "captured only {played_secs:.1}s of {PLAY_SECS:.0}s — player starved/stalled",
    );

    let scan_from = (scan_from_frame as usize * chan).min(pcm.len());
    let sine = SinePhaseSpec {
        freq_hz: FREQ_HZ,
        sample_rate: out_rate,
        channels: CHANNELS,
    };
    let drifts: Vec<PhaseDrift> = scan_rendered_pcm(&pcm[scan_from..], sine, SCAN_INTERVAL_FRAMES);

    assert!(
        drifts.is_empty(),
        "rendered FLAC playback has {} phase discontinuit(ies) (scenario={scenario:?} backend={backend:?} delay_ms={delay_ms:?}): {drifts:?}",
        drifts.len(),
    );
}

/// Phase-continuity guard for the real player loop (`PlayerProcessor` /
/// `PlayerTrack` / `PlayerResource` — the same path cpal drives), driven
/// offline via `OfflinePlayer::render`. Captures the rendered stereo output
/// and scans it for any timeline seam: a forward content skip reads as a
/// positive phase jump, uncompensated injected silence/lag as a negative one.
///
/// Built to hunt the reported production symptom — a 1–5 s forward position
/// jump every 10–30 s during sustained FLAC playback (Manual lossless, single
/// track) on the desktop cpal player.
///
/// `#[ignore]`d because it is **not yet deterministic**: when the host is idle
/// the paced render stays continuous (green), but under machine load the ~4×
/// pacing starves the decode worker and trips the player's underrun path,
/// producing phase discontinuities (observed: ~40 jumps, mostly negative = the
/// by-design zero-fill lag, plus a few positive = forward skips). That makes it
/// a load-dependent flake, not a hard gate. The discontinuities are real, so it
/// is a lead — but a clean RED needs a guaranteed-underrun trigger (large
/// `DelayRule` that drains the buffer at a known point) rather than an
/// ambient-load race. Unignore once the underrun is forced deterministically.
/// Run with `--run-ignored`.
#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(60)))]
#[ignore = "load-dependent flake, not yet deterministic — paced offline render starves decode under host load and trips the player underrun path; needs a forced-underrun trigger to become a reliable RED for the production forward-skip"]
#[case::sustained_flac_symphonia(Scenario::SustainedFlac, DecoderBackend::Symphonia, None, 44_100)]
#[case::switch_to_flac_symphonia(Scenario::SwitchToFlac, DecoderBackend::Symphonia, None, 44_100)]
#[case::switch_to_flac_symphonia_delay(
    Scenario::SwitchToFlac,
    DecoderBackend::Symphonia,
    Some(150),
    44_100
)]
// Host output rate 48 kHz ≠ content 44.1 kHz: activates the playback-pipeline
// resampler, the dominant real-cpal condition absent from same-rate offline
// pulls. This is where a periodic forward jump during sustained FLAC would live.
#[case::sustained_flac_symphonia_resamp(
    Scenario::SustainedFlac,
    DecoderBackend::Symphonia,
    None,
    48_000
)]
#[case::switch_to_flac_symphonia_resamp(
    Scenario::SwitchToFlac,
    DecoderBackend::Symphonia,
    None,
    48_000
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::switch_to_flac_apple_resamp(Scenario::SwitchToFlac, DecoderBackend::Apple, None, 48_000)
)]
async fn flac_realtime_player_continuity(
    #[case] scenario: Scenario,
    #[case] backend: DecoderBackend,
    #[case] delay_ms: Option<u64>,
    #[case] out_rate: u32,
) {
    run_case(scenario, backend, delay_ms, out_rate).await;
}
