#![cfg(not(target_arch = "wasm32"))]
#![forbid(unsafe_code)]

use std::{num::NonZeroU32, sync::Arc};

use kithara_assets::StoreOptions;
use kithara_platform::time::{Duration, Instant, sleep};
use kithara_play::{
    PlayerConfig, PlayerEvent, Resource, ResourceConfig, impls::offline_backend::OfflineConfig,
};
use kithara_test_utils::{
    HlsFixtureBuilder, TestServerHelper, TestTempDir,
    fixture_protocol::{PackagedAudioRequest, PackagedAudioSource, PackagedSignal},
    temp_dir,
};

use super::offline_player_harness::OfflinePlayerHarness;
use crate::gapless_common::{
    AAC_GAPLESS_ENCODER_DELAY, AAC_GAPLESS_SEGMENT_SECS, AAC_GAPLESS_SEGMENTS,
    AAC_GAPLESS_TRAILING_DELAY, GAPLESS_CHANNELS, GAPLESS_SAMPLE_RATE,
};

const BLOCK_FRAMES: u32 = 512;
const POST_ROLL_BLOCKS: usize = 8;
const BOUNDARY_WINDOW_MS: usize = 5;
/// Anything below this absolute amplitude (after AAC re-encode) is treated as
/// silence. Picked low enough to ignore quantisation noise and high enough
/// that genuine sine peaks are never classified as silence.
const SILENCE_THRESHOLD: f32 = 1.0e-3;

#[kithara::test(
    native,
    tokio,
    timeout(Duration::from_secs(30)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
async fn seamless_queue_advance_gapless_when_crossfade_is_zero(temp_dir: TestTempDir) {
    let server = TestServerHelper::new().await;
    let expected_visible_frames = crate::gapless_common::generated_aac_elst_visible_frames();
    let harness = OfflinePlayerHarness::new(
        PlayerConfig::default().with_crossfade_duration(0.0),
        OfflineConfig {
            sample_rate: GAPLESS_SAMPLE_RATE,
            block_frames: BLOCK_FRAMES,
        },
    );
    let first = create_gapless_hls_resource(
        harness.player(),
        &server,
        temp_dir.path(),
        "item-1",
        PackagedSignal::Sine { freq_hz: 1_000.0 },
    )
    .await;
    let second = create_gapless_hls_resource(
        harness.player(),
        &server,
        temp_dir.path(),
        "item-2",
        PackagedSignal::Sine { freq_hz: 1_000.0 },
    )
    .await;

    load_tagged_queue(&harness, [first, second]);

    let (rendered, events) = render_until_item_end(&harness, "item-2").await;
    let left = deinterleave_left(&rendered, usize::from(GAPLESS_CHANNELS));
    let sample_rate = usize::try_from(GAPLESS_SAMPLE_RATE).expect("sample rate fits usize");

    let item1_end_event = timed_event_frame_end(&events, |event| {
        matches!(
            event,
            PlayerEvent::ItemDidPlayToEnd { item_id: Some(id) } if id == "item-1"
        )
    })
    .expect("first item must emit ItemDidPlayToEnd before the queue completes");
    let item2_end_event = timed_event_frame_end(&events, |event| {
        matches!(
            event,
            PlayerEvent::ItemDidPlayToEnd { item_id: Some(id) } if id == "item-2"
        )
    })
    .expect("second item must emit ItemDidPlayToEnd before the queue completes");

    // Total joined visible length must approximate two full items. The
    // tolerance covers AAC encoder/decoder rounding and offline-render block
    // alignment (one block ≈ 10 ms at 48 kHz).
    let combined_visible = expected_visible_frames.saturating_mul(2);
    let combined_actual = item2_end_event;
    let combined_delta = combined_actual.abs_diff(combined_visible);
    assert!(
        combined_delta <= sample_rate / 10,
        "joined visible length should approximate {combined_visible} frames \
         (got {combined_actual}, delta {combined_delta} > tolerance \
         {tolerance}); events={events:?}",
        tolerance = sample_rate / 10
    );

    // Practically gapless: the RT-side handover must keep stuffing PCM into
    // the same render block where item-1 hits EOF. We allow at most a
    // sub-block silence run and require the boundary energy to stay close
    // to the sustained level.
    let search_start = item1_end_event.saturating_sub(BLOCK_FRAMES as usize * 2);
    let search_end = item1_end_event
        .saturating_add(BLOCK_FRAMES as usize)
        .min(left.len());
    let max_silence = max_silence_run(&left, search_start, search_end);
    let max_silence_ms_x10 = max_silence * 10_000 / sample_rate.max(1);
    assert!(
        max_silence < BLOCK_FRAMES as usize,
        "boundary silence run must stay below one render block ({block} frames), \
         got {max_silence} frames (~{ms_int}.{ms_frac:01} ms) in [{search_start}..{search_end}); \
         events={events:?}",
        block = BLOCK_FRAMES as usize,
        ms_int = max_silence_ms_x10 / 10,
        ms_frac = max_silence_ms_x10 % 10
    );

    let dip = worst_boundary_energy_dip_db(
        &left,
        search_start,
        search_end,
        sample_rate,
        BOUNDARY_WINDOW_MS,
    );
    assert!(
        dip < 1.5,
        "boundary energy dip must stay below 1.5 dB, got {dip:.3} dB \
         in [{search_start}..{search_end}); events={events:?}"
    );
}

#[kithara::test(
    native,
    tokio,
    timeout(Duration::from_secs(30)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
async fn seamless_queue_advance_overlaps_tracks_when_crossfade_is_non_zero(temp_dir: TestTempDir) {
    let server = TestServerHelper::new().await;
    let expected_visible_frames = crate::gapless_common::generated_aac_elst_visible_frames();
    let harness = OfflinePlayerHarness::new(
        PlayerConfig::default().with_crossfade_duration(1.0),
        OfflineConfig {
            sample_rate: GAPLESS_SAMPLE_RATE,
            block_frames: BLOCK_FRAMES,
        },
    );
    let first = create_gapless_hls_resource(
        harness.player(),
        &server,
        temp_dir.path(),
        "item-1",
        PackagedSignal::Sine { freq_hz: 440.0 },
    )
    .await;
    let second = create_gapless_hls_resource(
        harness.player(),
        &server,
        temp_dir.path(),
        "item-2",
        PackagedSignal::Sine { freq_hz: 880.0 },
    )
    .await;

    load_tagged_queue(&harness, [first, second]);

    let (rendered, events) = render_until_item_end(&harness, "item-2").await;
    let left = deinterleave_left(&rendered, usize::from(GAPLESS_CHANNELS));
    let sample_rate = usize::try_from(GAPLESS_SAMPLE_RATE).expect("sample rate fits usize");

    let item1_end = timed_event_frame_end(&events, |event| {
        matches!(
            event,
            PlayerEvent::ItemDidPlayToEnd { item_id: Some(id) } if id == "item-1"
        )
    })
    .expect("first item must emit ItemDidPlayToEnd before the queue completes");
    let _ = expected_visible_frames;
    let item2_activated = events
        .iter()
        .filter(|timed| matches!(&timed.event, PlayerEvent::CurrentItemChanged))
        .nth(1)
        .map(|timed| timed.frame_end)
        .expect("CurrentItemChanged should fire when item-2 takes over");

    assert!(
        item2_activated < item1_end,
        "item-2 must take over before item-1 reaches EOF for a real overlap; \
         item2_activated={item2_activated}, item1_end={item1_end}, events={events:?}"
    );

    // Take the actual overlap window the runtime produced — from when item-2
    // is activated to when item-1 reports EOF. That is the only segment where
    // both signals can coexist by construction.
    let overlap_start = item2_activated;
    let overlap_end = item1_end.min(left.len());
    assert!(
        overlap_end > overlap_start,
        "overlap window must be non-empty; \
         overlap=[{overlap_start}..{overlap_end}), events={events:?}"
    );

    let leading_solo_start = overlap_start.saturating_sub(sample_rate / 2);
    let leading_solo_end = overlap_start;
    let trailing_solo_start = overlap_end;
    let trailing_solo_end = trailing_solo_start
        .saturating_add(sample_rate / 2)
        .min(left.len());

    let overlap_440 = goertzel_magnitude(&left[overlap_start..overlap_end], 440.0, sample_rate);
    let overlap_880 = goertzel_magnitude(&left[overlap_start..overlap_end], 880.0, sample_rate);
    let solo_440 = goertzel_magnitude(
        &left[leading_solo_start..leading_solo_end],
        440.0,
        sample_rate,
    );
    let solo_880 = goertzel_magnitude(
        &left[trailing_solo_start..trailing_solo_end],
        880.0,
        sample_rate,
    );

    // Both signals must be clearly audible during overlap. The 0.2 ratio
    // tolerates the equal-power crossfade (which attenuates each leg by up
    // to ~3 dB at the midpoint) plus a margin for AAC quantisation.
    assert!(
        overlap_440 >= solo_440 * 0.2,
        "leading frequency must remain audible in overlap window; \
         overlap_440={overlap_440:.3}, solo_440={solo_440:.3}, events={events:?}"
    );
    assert!(
        overlap_880 >= solo_880 * 0.2,
        "trailing frequency must be audible before the first item reaches EOF; \
         overlap_880={overlap_880:.3}, solo_880={solo_880:.3}, events={events:?}"
    );

    // Constant-power crossfade: at any sub-window inside the overlap, the
    // mixed RMS must stay close to the surrounding solo RMS. We sweep small
    // windows to catch local dips that an aggregate RMS would smear over.
    let dip = worst_overlap_rms_dip_db(
        &left,
        overlap_start,
        overlap_end,
        leading_solo_start,
        leading_solo_end,
        trailing_solo_start,
        trailing_solo_end,
        sample_rate,
        BOUNDARY_WINDOW_MS * 20,
    );
    assert!(
        dip < 1.5,
        "overlap RMS dip must stay below 1.5 dB, got {dip:.3} dB \
         (overlap=[{overlap_start}..{overlap_end}), events={events:?})"
    );
    let max_silence = max_silence_run(&left, overlap_start, overlap_end);
    assert!(
        max_silence < BLOCK_FRAMES as usize,
        "no extended silence allowed inside the overlap; \
         max_silence={max_silence} frames, overlap=[{overlap_start}..{overlap_end}), \
         events={events:?}"
    );
}

#[allow(dead_code)]
async fn create_gapless_hls_resource(
    player: &kithara_play::PlayerImpl,
    server: &TestServerHelper,
    cache_dir: &std::path::Path,
    item_id: &'static str,
    signal: PackagedSignal,
) -> (Resource, Arc<str>) {
    let source = PackagedAudioSource::Signal(signal);
    let created = server
        .create_hls(
            HlsFixtureBuilder::new()
                .variant_count(1)
                .segments_per_variant(AAC_GAPLESS_SEGMENTS)
                .segment_duration_secs(AAC_GAPLESS_SEGMENT_SECS)
                .packaged_audio(PackagedAudioRequest {
                    codec: kithara_stream::AudioCodec::AacLc,
                    sample_rate: GAPLESS_SAMPLE_RATE,
                    channels: GAPLESS_CHANNELS,
                    timescale: Some(GAPLESS_SAMPLE_RATE),
                    bit_rate: Some(128_000),
                    encoder_delay: NonZeroU32::new(AAC_GAPLESS_ENCODER_DELAY),
                    trailing_delay: NonZeroU32::new(AAC_GAPLESS_TRAILING_DELAY),
                    source,
                    variant_overrides: Vec::new(),
                }),
        )
        .await
        .expect("create seamless queue HLS fixture");

    let item_id = Arc::<str>::from(item_id);
    let store = StoreOptions::new(cache_dir);
    let mut config = ResourceConfig::new(created.master_url().as_str())
        .expect("valid HLS master URL")
        .with_store(store);
    player.prepare_config(&mut config);
    let mut resource = Resource::new(config)
        .await
        .expect("open HLS resource for seamless queue fixture");
    // Force the first decoded chunk to land in the resource's internal buffer
    // before we hand it to the player. Without this, the audio thread races the
    // HLS downloader/decoder for the first ~50–500 ms of playback and feeds
    // silence to the mixer, which kills crossfade energy and creates an audible
    // gap at the join. The contract under test is queue auto-advance — not
    // first-chunk preroll — so paying that latency up front mirrors what real
    // callers do via the public `Resource::preload()` API.
    resource.preload().await;
    eprintln!(
        "[DBG] resource ready: item={item_id} duration_secs={:?} visible_frames={}",
        resource.duration(),
        crate::gapless_common::generated_aac_elst_visible_frames()
    );
    (resource, item_id)
}

fn load_tagged_queue<const N: usize>(
    harness: &OfflinePlayerHarness,
    items: [(Resource, Arc<str>); N],
) {
    harness.player().reserve_slots(items.len());
    for (index, (resource, item_id)) in items.into_iter().enumerate() {
        harness
            .player()
            .replace_item_tagged(index, resource, Some(item_id));
    }
    harness
        .player()
        .select_item(0, true)
        .expect("select first queue item");
}

async fn render_until_item_end(
    harness: &OfflinePlayerHarness,
    terminal_item_id: &'static str,
) -> (Vec<f32>, Vec<TimedPlayerEvent>) {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut rendered = Vec::new();
    let mut rendered_frames = 0usize;
    let mut events = Vec::new();

    loop {
        let block = harness.render(BLOCK_FRAMES as usize);
        rendered.extend_from_slice(&block);
        rendered_frames = rendered_frames.saturating_add(BLOCK_FRAMES as usize);
        events.extend(
            harness
                .tick_and_drain()
                .into_iter()
                .map(|event| TimedPlayerEvent {
                    frame_end: rendered_frames,
                    event,
                }),
        );

        if events.iter().any(|event| {
            matches!(
                &event.event,
                PlayerEvent::ItemDidPlayToEnd { item_id: Some(id) }
                    if id == terminal_item_id
            )
        }) {
            for _ in 0..POST_ROLL_BLOCKS {
                let block = harness.render(BLOCK_FRAMES as usize);
                rendered.extend_from_slice(&block);
                rendered_frames = rendered_frames.saturating_add(BLOCK_FRAMES as usize);
                events.extend(
                    harness
                        .tick_and_drain()
                        .into_iter()
                        .map(|event| TimedPlayerEvent {
                            frame_end: rendered_frames,
                            event,
                        }),
                );
            }
            return (rendered, events);
        }

        assert!(
            Instant::now() <= deadline,
            "timed out waiting for queue to finish; events={events:?}"
        );
        sleep(Duration::from_millis(5)).await;
    }
}

fn deinterleave_left(samples: &[f32], channels: usize) -> Vec<f32> {
    samples
        .chunks_exact(channels)
        .map(|frame| frame[0])
        .collect::<Vec<_>>()
}

#[derive(Clone, Debug)]
struct TimedPlayerEvent {
    frame_end: usize,
    event: PlayerEvent,
}

fn timed_event_frame_end<P>(events: &[TimedPlayerEvent], predicate: P) -> Option<usize>
where
    P: Fn(&PlayerEvent) -> bool,
{
    events
        .iter()
        .find(|timed| predicate(&timed.event))
        .map(|timed| timed.frame_end)
}

/// Longest run of near-zero samples in `[start..end)`. Used to detect audible
/// gaps without depending on phase alignment.
fn max_silence_run(samples: &[f32], start: usize, end: usize) -> usize {
    let end = end.min(samples.len());
    if end <= start {
        return 0;
    }
    let mut max_run = 0usize;
    let mut current = 0usize;
    for sample in &samples[start..end] {
        if sample.abs() < SILENCE_THRESHOLD {
            current += 1;
            if current > max_run {
                max_run = current;
            }
        } else {
            current = 0;
        }
    }
    max_run
}

/// Sweep a window across `[search_start..search_end)` and return the worst
/// (largest) energy dip in dB. The reference is the surrounding RMS just
/// outside the search range so we measure transient drops, not steady-state
/// volume changes.
fn worst_boundary_energy_dip_db(
    samples: &[f32],
    search_start: usize,
    search_end: usize,
    sample_rate: usize,
    window_ms: usize,
) -> f64 {
    let window_frames = sample_rate.saturating_mul(window_ms).div_ceil(1_000).max(1);
    let search_end = search_end.min(samples.len());
    if search_end <= search_start || search_end - search_start < window_frames {
        return 0.0;
    }

    // Reference RMS: a half-second window each side of the search range,
    // clipped to the rendered buffer. Both items play the same sine here
    // so either side is a fair baseline.
    let ref_span = sample_rate / 2;
    let left_ref = rms(samples, search_start.saturating_sub(ref_span), search_start);
    let right_ref = rms(
        samples,
        search_end,
        (search_end + ref_span).min(samples.len()),
    );
    let reference = left_ref.max(right_ref);
    if reference <= f64::EPSILON {
        return 0.0;
    }

    let mut worst = 0.0_f64;
    let mut start = search_start;
    while start + window_frames <= search_end {
        let window = rms(samples, start, start + window_frames);
        if window < reference {
            let dip = 20.0 * (reference / window.max(f64::EPSILON)).log10();
            if dip > worst {
                worst = dip;
            }
        }
        start += window_frames / 2; // 50% overlap
    }
    worst
}

#[expect(
    clippy::too_many_arguments,
    reason = "windowed RMS scan needs explicit reference + overlap bounds"
)]
fn worst_overlap_rms_dip_db(
    samples: &[f32],
    overlap_start: usize,
    overlap_end: usize,
    left_start: usize,
    left_end: usize,
    right_start: usize,
    right_end: usize,
    sample_rate: usize,
    window_ms: usize,
) -> f64 {
    let window_frames = sample_rate.saturating_mul(window_ms).div_ceil(1_000).max(1);
    let overlap_end = overlap_end.min(samples.len());
    if overlap_end <= overlap_start {
        return 0.0;
    }
    let reference = rms(samples, left_start, left_end).max(rms(samples, right_start, right_end));
    if reference <= f64::EPSILON {
        return 0.0;
    }

    let mut worst = 0.0_f64;
    let mut start = overlap_start;
    while start + window_frames <= overlap_end {
        let window = rms(samples, start, start + window_frames);
        if window < reference {
            let dip = 20.0 * (reference / window.max(f64::EPSILON)).log10();
            if dip > worst {
                worst = dip;
            }
        }
        start += window_frames / 2;
    }
    worst
}

fn rms(samples: &[f32], start: usize, end: usize) -> f64 {
    if end <= start {
        return 0.0;
    }

    let sum = samples[start..end]
        .iter()
        .map(|sample| {
            let sample = f64::from(*sample);
            sample * sample
        })
        .sum::<f64>();
    (sum / (end - start) as f64).sqrt()
}

fn goertzel_magnitude(samples: &[f32], freq_hz: f64, sample_rate: usize) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }

    let omega = 2.0 * std::f64::consts::PI * freq_hz / sample_rate as f64;
    let coeff = 2.0 * omega.cos();
    let mut q1 = 0.0f64;
    let mut q2 = 0.0f64;

    for sample in samples {
        let q0 = coeff * q1 - q2 + f64::from(*sample);
        q2 = q1;
        q1 = q0;
    }

    let real = q1 - q2 * omega.cos();
    let imag = q2 * omega.sin();
    (real * real + imag * imag).sqrt()
}
