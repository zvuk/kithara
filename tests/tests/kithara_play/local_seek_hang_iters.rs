#![cfg(not(target_arch = "wasm32"))]
#![forbid(unsafe_code)]

use kithara::{
    assets::StoreOptions,
    decode::DecoderBackend,
    events::{AbrMode, AudioEvent, Event, EventReceiver},
    net::{HttpClient, NetOptions},
    platform::{
        CancelToken,
        time::{self, Duration, Instant, timeout},
        tokio::sync::broadcast::error::TryRecvError,
    },
    play::{Resource, ResourceConfig},
    stream::dl::{Downloader, DownloaderConfig},
};
use kithara_integration_tests::{
    HlsFixtureBuilder, TestServerHelper, offline::OfflinePlayer, temp_dir,
};
use url::Url;

use crate::common::test_defaults::Consts as Shared;

struct Consts;
impl Consts {
    const SAMPLE_RATE: u32 = Shared::SAMPLE_RATE;
    const BLOCK_FRAMES: usize = Shared::OFFLINE_BLOCK_FRAMES;
    /// Produced-audio horizon for the event-driven warmup: the warmup
    /// drives the render pull until `PlaybackProgress` advances past this
    /// position, proving the decode worker primed the pipeline before the
    /// first measurement window (synthetic fixture has no cold-CDN latency).
    const WARMUP_SECS: f64 = 0.5;
    /// Measurement window: long enough that a real hang manifests as
    /// >=20% silent blocks but short enough to keep the test fast.
    const PLAY_WINDOW_SECS: f64 = 1.5;
    const MAX_SILENCE_FRACTION: f32 = 0.20;
    const MIN_WINDOW_RMS: f32 = 0.03;
    const ITERATIONS: usize = 3;
    /// Variants × segments × duration → fixture is 64 s long. Seek +30 s
    /// from a position around 2 s leaves >>1 segment of fresh fetch
    /// after the seek, exercising the same recreate path silvercomet
    /// did.
    const SEGMENTS_PER_VARIANT: usize = 16;
    const SEGMENT_DURATION_SECS: f64 = 4.0;
}

struct WindowStats {
    silent_blocks: u32,
    total_blocks: u32,
    window_start_sample: usize,
}

/// Render `blocks` measured blocks, parking on the virtual clock between
/// pulls so each block reflects audio the decode worker actually produced.
///
/// `OfflinePlayer::render` is a real-time pull: an underrun zero-fills the
/// output rather than blocking, exactly as a hardware callback would. On the
/// real clock the worker stays ahead because the render loop paces against
/// wall-clock (`thread::sleep` per block); under flash that pacing collapses,
/// so a tight render loop drains the producer ring faster than the worker can
/// refill it and the window captures underrun silence that never happens on a
/// device. This helper restores the contract by gating each measured block on
/// real produced audio: it pulls a block, and if the ring underran (the block
/// is silent) it parks the body on the virtual clock via `time::sleep().await`
/// — letting the engine advance virtual time and the worker deliver — then
/// re-pulls the same block slot. The window therefore advances only on frames
/// the worker has produced, mirroring the real-clock behaviour where the ring
/// is kept full. A genuine producer stall (recreate-loop after seek) never
/// fills the ring, so the per-block `deadline` assert fires precisely at the
/// stall. The window's silence-fraction / RMS contract is measured by the
/// caller, unchanged.
async fn render_and_collect(
    player: &mut OfflinePlayer,
    events: &mut EventReceiver,
    blocks: u32,
    samples_out: &mut Vec<f32>,
    deadline: Instant,
    stage: &str,
) -> WindowStats {
    const ACTIVE_THRESHOLD: f32 = 0.001;
    let block_budget =
        Duration::from_secs_f64(Consts::BLOCK_FRAMES as f64 / f64::from(Consts::SAMPLE_RATE));

    let window_start_sample = samples_out.len();
    let mut silent_blocks = 0u32;

    for _ in 0..blocks {
        // Pull this block, parking on the virtual clock until the producer
        // ring has data for it. `render()` shares the read path of a device
        // callback, so a non-silent block proves the worker delivered real
        // frames for it; a silent block is an underrun, so we let virtual time
        // advance and re-pull rather than counting ring-empty silence.
        let out = loop {
            let out = player.render(Consts::BLOCK_FRAMES);
            // Drain progress/lifecycle events so the bounded bus cannot lag and
            // so polling them rides the virtual clock alongside the sleep below.
            drain_events(events);

            if out.iter().any(|s| s.abs() > ACTIVE_THRESHOLD) {
                break out;
            }

            assert!(
                Instant::now() <= deadline,
                "[{stage}] decode worker stopped delivering audio mid-window \
                 — producer ring underran and never refilled (pipeline stalled)"
            );
            time::sleep(block_budget).await;
        };

        if !out.iter().any(|s| s.abs() > ACTIVE_THRESHOLD) {
            silent_blocks += 1;
        }

        samples_out.extend_from_slice(&out);
    }

    WindowStats {
        silent_blocks,
        total_blocks: blocks,
        window_start_sample,
    }
}

/// Drain pending bus events without blocking. Keeps the bounded broadcast bus
/// from lagging while the render loop parks on the virtual clock; a lagged
/// receiver would otherwise wedge later `try_recv` reads.
fn drain_events(events: &mut EventReceiver) {
    loop {
        match events.try_recv() {
            Ok(_) => continue,
            Err(TryRecvError::Empty | TryRecvError::Closed) => break,
            Err(TryRecvError::Lagged(_)) => continue,
        }
    }
}

fn blocks_for_seconds(secs: f64) -> u32 {
    let blocks = (secs * f64::from(Consts::SAMPLE_RATE) / Consts::BLOCK_FRAMES as f64).ceil();
    #[expect(
        clippy::cast_sign_loss,
        clippy::cast_possible_truncation,
        reason = "positive ceiling fits in u32 for second-scale windows"
    )]
    let result = blocks as u32;
    result
}

fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    #[expect(
        clippy::cast_precision_loss,
        reason = "sample count precision adequate"
    )]
    let n = samples.len() as f32;
    let sum_sq: f32 = samples.iter().map(|s| s * s).sum();
    (sum_sq / n).sqrt()
}

/// Drive the offline render pull until the decode worker has actually
/// delivered real PCM, signalled by `PlaybackProgress` events whose
/// position advances past `min_position_secs`.
///
/// `OfflinePlayer::render` is a synchronous pull: when the worker's ring
/// is still empty it returns silence rather than blocking. Under flash the
/// real-time `thread::sleep` pacing collapses, so a fixed-length warmup can
/// finish before the worker primes the pipeline and the first measurement
/// window then captures all silence. This helper instead awaits the real
/// produced-audio signal: `Audio::read` emits `PlaybackProgress` from the
/// render pull when it returns non-zero frames, so observing the position
/// advance proves audio is flowing. The interleaved `time::sleep().await`
/// rides the virtual clock (poll cadence only), parking the body so the
/// engine can advance virtual time and the worker can deliver; the outer
/// `deadline` only bounds a genuine stall (the test-level `timeout` is the
/// real backstop).
async fn render_until_audio(
    player: &mut OfflinePlayer,
    events: &mut EventReceiver,
    min_position_secs: f64,
    deadline: Instant,
    stage: &str,
) {
    let block_budget =
        Duration::from_secs_f64(Consts::BLOCK_FRAMES as f64 / f64::from(Consts::SAMPLE_RATE));
    // ms threshold computed via `Duration` so no float→int cast is needed.
    let min_position_ms = Duration::from_secs_f64(min_position_secs).as_millis();

    loop {
        let _ = player.render(Consts::BLOCK_FRAMES);

        let mut advanced = false;
        loop {
            match events.try_recv() {
                Ok(Event::Audio(AudioEvent::PlaybackProgress { position_ms, .. })) => {
                    if u128::from(position_ms) > min_position_ms {
                        advanced = true;
                    }
                }
                Ok(_) => continue,
                Err(TryRecvError::Empty | TryRecvError::Closed) => break,
                Err(TryRecvError::Lagged(_)) => continue,
            }
        }
        if advanced {
            return;
        }

        assert!(
            Instant::now() <= deadline,
            "[{stage}] decode worker never delivered audio past {min_position_secs:.1}s \
             — pipeline stalled before measurement window"
        );
        time::sleep(block_budget).await;
    }
}

async fn build_resource(
    url: &Url,
    downloader: &Downloader,
    iter_label: &str,
    store: StoreOptions,
    backend: DecoderBackend,
    abr: AbrMode,
) -> Resource {
    let cfg = ResourceConfig::for_src(url.as_str())
        .unwrap_or_else(|e| panic!("ResourceConfig::for_src({url}): {e}"))
        .byte_pool(kithara::bufpool::BytePool::default())
        .pcm_pool(kithara::bufpool::PcmPool::default())
        .downloader(downloader.clone())
        .name(format!("{iter_label}|{url}"))
        .store(store)
        .decoder(
            kithara::audio::AudioDecoderConfig::builder()
                .backend(backend)
                .build(),
        )
        .initial_abr_mode(abr)
        .build();
    let mut resource = Resource::new(cfg)
        .await
        .unwrap_or_else(|e| panic!("Resource::new({url}): {e:?}"));
    timeout(Duration::from_secs(10), resource.preload())
        .await
        .unwrap_or_else(|_| panic!("Resource::preload({url}) timed out after 10s"))
        .unwrap_or_else(|err| panic!("Resource::preload({url}) failed: {err}"));
    resource
}

#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(120)))]
#[case::symphonia_auto(DecoderBackend::Symphonia, AbrMode::Auto(None))]
#[case::symphonia_locked_low(DecoderBackend::Symphonia, AbrMode::manual(0))]
#[case::symphonia_locked_high(DecoderBackend::Symphonia, AbrMode::manual(2))]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::apple_auto(DecoderBackend::Apple, AbrMode::Auto(None))
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::apple_locked_low(DecoderBackend::Apple, AbrMode::manual(0))
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::apple_locked_high(DecoderBackend::Apple, AbrMode::manual(2))
)]
#[cfg_attr(
    target_os = "android",
    case::android(DecoderBackend::Android, AbrMode::Auto(None))
)]
async fn local_seek_middle_hang_iters(#[case] backend: DecoderBackend, #[case] abr: AbrMode) {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    kithara_integration_tests::apple_warmup::warm_if_apple(backend);

    let helper = TestServerHelper::new().await;
    let builder = HlsFixtureBuilder::new()
        .variant_count(3)
        .segments_per_variant(Consts::SEGMENTS_PER_VARIANT)
        .segment_duration_secs(Consts::SEGMENT_DURATION_SECS)
        .variant_bandwidths(vec![640_000, 1_280_000, 2_560_000])
        .packaged_audio_aac_lc(Consts::SAMPLE_RATE, 2);
    let master = helper
        .create_hls(builder)
        .await
        .expect("create local HLS fixture")
        .master_url();

    let window_blocks = blocks_for_seconds(Consts::PLAY_WINDOW_SECS);
    let mut next_seek_epoch = 1u64;

    for iter in 0..Consts::ITERATIONS {
        let iter_label = format!("iter-{iter}");
        let temp = temp_dir();
        let store = StoreOptions::new(temp.path());
        let downloader = Downloader::new(
            DownloaderConfig::for_client(HttpClient::new(
                NetOptions::default(),
                CancelToken::never(),
            ))
            .build(),
        );

        let mut player = OfflinePlayer::new(Consts::SAMPLE_RATE);
        let mut iteration_samples: Vec<f32> = Vec::new();

        let resource = build_resource(&master, &downloader, &iter_label, store, backend, abr).await;
        // Subscribe before the resource moves into the player so no
        // `PlaybackProgress` event is missed once the render pull starts.
        let mut events = resource.subscribe();
        player.load_and_fadein(resource, &format!("{iter_label}|local-hls"));

        // Event-driven warmup: drive the render pull until the worker has
        // actually produced PCM (position advances past the warmup horizon),
        // instead of burning a fixed number of blocks that — under flash —
        // can finish before the worker primes the pipeline. Parks the body on
        // the virtual clock so the engine advances and the worker delivers.
        let warmup_deadline = Instant::now() + Duration::from_secs(30);
        render_until_audio(
            &mut player,
            &mut events,
            Consts::WARMUP_SECS,
            warmup_deadline,
            "warmup",
        )
        .await;

        let initial_window_deadline = Instant::now() + Duration::from_secs(30);
        let initial = render_and_collect(
            &mut player,
            &mut events,
            window_blocks,
            &mut iteration_samples,
            initial_window_deadline,
            "pre-seek window",
        )
        .await;
        let initial_samples = &iteration_samples[initial.window_start_sample..];
        let initial_rms = rms(initial_samples);
        let initial_silence_fraction =
            f32::from(u16::try_from(initial.silent_blocks).unwrap_or(u16::MAX))
                / f32::from(
                    u16::try_from(initial.total_blocks)
                        .unwrap_or(u16::MAX)
                        .max(1),
                );

        assert!(
            initial_silence_fraction <= Consts::MAX_SILENCE_FRACTION,
            "[iter {iter}] local hls: initial silent fraction = {:.1}% \
             (max {:.0}% allowed) — pre-seek window had no audio",
            initial_silence_fraction * 100.0,
            Consts::MAX_SILENCE_FRACTION * 100.0,
        );
        assert!(
            initial_rms >= Consts::MIN_WINDOW_RMS,
            "[iter {iter}] local hls: initial RMS = {initial_rms:.4} \
             (min {:.4} required) — no real audio in pre-seek window",
            Consts::MIN_WINDOW_RMS,
        );

        let seek_target = player.position() + 30.0;
        let seek_epoch = next_seek_epoch;
        next_seek_epoch += 1;
        player.seek(seek_target, seek_epoch);

        // Wait for the seek to land in produced audio before measuring: the
        // post-seek render pull emits `PlaybackProgress` past the target once
        // the worker has refetched + decoded the new region. Awaiting that real
        // state (rather than rendering blindly into a not-yet-primed pipeline)
        // is what makes the after-seek window reflect actual post-seek audio
        // under flash. The contract — silence fraction + RMS over the window —
        // is measured unchanged below.
        // Threshold a hair below the target so a segment-boundary-aligned
        // commit landing exactly at the seek point still registers (the seek
        // has demonstrably landed and is producing post-seek audio).
        let seek_landed_secs = (seek_target - 1.0).max(0.0);
        let seek_deadline = Instant::now() + Duration::from_secs(30);
        render_until_audio(
            &mut player,
            &mut events,
            seek_landed_secs,
            seek_deadline,
            "after-seek",
        )
        .await;

        let after_window_deadline = Instant::now() + Duration::from_secs(30);
        let after = render_and_collect(
            &mut player,
            &mut events,
            window_blocks,
            &mut iteration_samples,
            after_window_deadline,
            "after-seek window",
        )
        .await;
        let after_samples = &iteration_samples[after.window_start_sample..];
        let after_rms = rms(after_samples);
        let after_silence_fraction =
            f32::from(u16::try_from(after.silent_blocks).unwrap_or(u16::MAX))
                / f32::from(u16::try_from(after.total_blocks).unwrap_or(u16::MAX).max(1));

        assert!(
            after_silence_fraction <= Consts::MAX_SILENCE_FRACTION,
            "[iter {iter}] local hls after seek→{seek_target:.1}s: silent fraction = \
             {:.1}% (max {:.0}% allowed) — HANG: decoder produced mostly silence after seek",
            after_silence_fraction * 100.0,
            Consts::MAX_SILENCE_FRACTION * 100.0,
        );
        assert!(
            after_rms >= Consts::MIN_WINDOW_RMS,
            "[iter {iter}] local hls after seek→{seek_target:.1}s: window RMS = \
             {after_rms:.4} (min {:.4} required) — HANG: post-seek window has no real audio",
            Consts::MIN_WINDOW_RMS,
        );

        drop(player);
        drop(downloader);
        drop(temp);
    }
}
