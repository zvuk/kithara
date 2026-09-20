#![cfg(not(target_arch = "wasm32"))]

//! Seam contract of a deck playing a queue: the configured crossfade length is
//! honoured, deck-level timestretch persists into the next track, and without a
//! grid the stream stays continuous at the original tempo.

use std::{collections::BTreeMap, num::NonZeroU32};

use kithara::{
    analysis::{AnalysisFile, AnalysisFingerprint},
    events::TrackId,
    platform::{time::Duration, tokio::task::spawn_blocking},
    warp::{AssetAxis, BeatGridState, SessionFrame, SyncAdmission},
};
use kithara_integration_tests::{
    audio_artifact::write_audio_artifact,
    cochlea::{
        CochleaReport, host_beat_alignment_failures, marked_rhythm_markers,
        marked_synchronization_failures, synchronization_failures, time_stretch_failures,
    },
    kithara,
    usdt_trace::{self, ProbeEvent},
};
use kithara_test_fixtures::{asset::Asset, assets};
use kithara_test_utils::probe::IntoProbeArg;

use super::sync_product_matrix::{
    DOWNTEMPO_HOUSE_PROVIDER, DOWNTEMPO_HOUSE_SYNC, ProductHarness, Provider, SyncCase,
};

struct Fixture;

impl Fixture {
    const SAMPLE_RATE: u32 = 48_000;
    const CHANNELS: u16 = 2;
    const FADE_SECS: f32 = 1.0;
    const RATE: f32 = 1.25;
    const RHYTHM_HOUSE_124: &str = "rhythm_wav_house_124_aligned";
    const RHYTHM_TECHNO_132: &str = "rhythm_wav_techno_132_aligned";
    const HOUSE_THEN_TECHNO: &[&str] = &[Self::RHYTHM_HOUSE_124, Self::RHYTHM_TECHNO_132];
    const SOLO_HOUSE: &[&str] = &[Self::RHYTHM_HOUSE_124];
    const SOLO_TECHNO: &[&str] = &[Self::RHYTHM_TECHNO_132];
    const SOLO: SyncCase = SyncCase::queued("solo", Self::SAMPLE_RATE, 1, 0.0);
    const SEAM_RATE_PERSISTS: SyncCase =
        SyncCase::queued("seam_rate_persists", Self::SAMPLE_RATE, 2, Self::FADE_SECS).paused();
    const SEAM_OFF: SyncCase = SyncCase::queued("seam_off", Self::SAMPLE_RATE, 2, Self::FADE_SECS);
    const SEAM_FADE_LENGTH: SyncCase =
        SyncCase::queued("seam_fade_length", Self::SAMPLE_RATE, 2, Self::FADE_SECS);
    const TRACK_GRID: SyncCase =
        SyncCase::queued("track_grid", Self::SAMPLE_RATE, 2, Self::FADE_SECS);
    const HOUSE_ANALYSIS: &str = "rhythm_expected_analysis_house_124_aligned";
    const HOST_BPM: f64 = 100.0;
    /// Five tracks whose authored tempos straddle the Host tempo, in
    /// alternating direction so every seam changes the stretch ratio.
    const FIVE_TEMPOS: &[(&str, &str, f64)] = &[
        (Self::RHYTHM_HOUSE_124, Self::HOUSE_ANALYSIS, 124.0),
        (
            "rhythm_wav_downtempo_96_aligned",
            "rhythm_expected_analysis_downtempo_96_aligned",
            96.0,
        ),
        (
            Self::RHYTHM_TECHNO_132,
            "rhythm_expected_analysis_techno_132_aligned",
            132.0,
        ),
        (
            "rhythm_wav_trip_hop_74_aligned",
            "rhythm_expected_analysis_trip_hop_74_aligned",
            74.0,
        ),
        (
            "rhythm_wav_breakbeat_140_aligned",
            "rhythm_expected_analysis_breakbeat_140_aligned",
            140.0,
        ),
    ];
    const FIVE_TRACK_NAMES: &[&str] = &[
        Self::RHYTHM_HOUSE_124,
        "rhythm_wav_downtempo_96_aligned",
        Self::RHYTHM_TECHNO_132,
        "rhythm_wav_trip_hop_74_aligned",
        "rhythm_wav_breakbeat_140_aligned",
    ];
    const FIVE_TRACK_QUEUE: SyncCase =
        SyncCase::queued("five_track_queue", Self::SAMPLE_RATE, 5, Self::FADE_SECS)
            .hold(Self::HOST_BPM)
            .paused();
    const ANALYSIS_FINGERPRINT: &str = "rhythm-fixture:v1";
}

/// The asset grid the fixture score promises for `wav`, read from its
/// analysis sidecar through the same bridge the product publishes from.
fn asset_grid(analysis: &str) -> kithara::warp::SegmentSet {
    let asset = assets::by_name(analysis).unwrap_or_else(|| panic!("missing `{analysis}`"));
    let fingerprint = AnalysisFingerprint::new(Some(Fixture::ANALYSIS_FINGERPRINT), None);
    AnalysisFile::parse(asset.bytes(), &fingerprint)
        .unwrap_or_else(|error| panic!("decode `{analysis}`: {error}"))
        .latest()
        .analysis()
        .beat_grid()
        .unwrap_or_else(|| panic!("`{analysis}` has no beat analysis"))
        .unwrap_or_else(|error| panic!("`{analysis}` is not a segment set: {error}"))
}

/// Session-axis span over which one track produced audio, from the `render`
/// probe: first and last block whose media clock advanced, and the media
/// frames it served in total.
#[derive(Clone, Copy, Debug)]
pub(super) struct TrackSpan {
    pub(super) first: i64,
    pub(super) last: i64,
    pub(super) served: u64,
    pub(super) output_frames: u64,
    source_free_frames: u64,
}

pub(super) fn spans(events: &[ProbeEvent]) -> BTreeMap<u64, TrackSpan> {
    let mut spans = BTreeMap::new();
    for event in events.iter().filter(|event| event.probe == "render") {
        let Some(base) = event.field("output_base").filter(|base| *base != u64::MAX) else {
            continue;
        };
        let Some(frames) = event.field("rendered_frames").filter(|frames| *frames > 0) else {
            continue;
        };
        let track = event.field("track_id").expect("render names its track");
        let offset = event.field("range_start").expect("render names its offset");
        let first = i64::from_probe_arg(base) + i64::from_probe_arg(offset);
        let last = first + i64::try_from(frames).expect("render frame count fits i64");
        let served = event
            .field("served_media_frames")
            .expect("render names its source frontier");
        spans
            .entry(track)
            .and_modify(|span: &mut TrackSpan| {
                span.first = span.first.min(first);
                span.last = span.last.max(last);
                if span.served == served {
                    span.source_free_frames += frames;
                }
                span.served = served;
                span.output_frames += frames;
            })
            .or_insert(TrackSpan {
                first,
                last,
                served,
                output_frames: frames,
                source_free_frames: 0,
            });
    }
    spans
}

/// Frames of a canonical 44-byte-header PCM WAV fixture.
pub(super) fn wav_frames(asset: &Asset) -> u64 {
    let bytes = asset.bytes();
    assert_eq!(&bytes[36..40], b"data", "fixture is a canonical PCM WAV");
    let block_align = u64::from(u16::from_le_bytes([bytes[32], bytes[33]]));
    let data_len = u64::from(u32::from_le_bytes([
        bytes[40], bytes[41], bytes[42], bytes[43],
    ]));
    data_len / block_align
}

fn track_len(name: &str) -> u64 {
    wav_frames(&assets::by_name(name).unwrap_or_else(|| panic!("missing `{name}`")))
}

fn total_frames(case: SyncCase, names: &[&str]) -> usize {
    let media: u64 = names.iter().map(|name| track_len(name)).sum();
    let fade = (f64::from(case.crossfade_secs) * f64::from(case.sample_rate)) as u64;
    usize::try_from(media + fade * 4).expect("frame budget fits usize")
}

fn block_frames(harness: &ProductHarness) -> i64 {
    i64::try_from(harness.block_frames).expect("block size fits i64")
}

/// Output frames of one bar at `bpm`.
fn bar_frames(bpm: f64) -> i64 {
    (f64::from(Fixture::SAMPLE_RATE) * 60.0 / bpm * 4.0).round() as i64
}

/// `half` frames before `center` and, `skip` frames after it, `half` frames
/// from there: the two sides of a seam. With `skip == 0` both sides start on
/// the same phase of any beat period that divides `half`.
fn seam_window(pcm: &[f32], center: i64, skip: i64, half: i64) -> (Vec<f32>, Vec<f32>) {
    let channels = i64::from(Fixture::CHANNELS);
    let at = |frame: i64| {
        usize::try_from((frame * channels).clamp(0, pcm.len() as i64)).expect("index fits")
    };
    (
        pcm[at(center - half)..at(center)].to_vec(),
        pcm[at(center + skip)..at(center + skip + half)].to_vec(),
    )
}

/// Every slice sits on `bpm` and all slices share beat and bar phase.
/// `marked` reads the score fixtures' exact markers; otherwise the tempo is
/// estimated, which is what real tracks need.
fn assert_in_phase(label: &str, slices: &[&[f32]], bpm: f64, marked: bool) {
    let oracle = if marked {
        marked_synchronization_failures
    } else {
        synchronization_failures
    };
    let failures = oracle(label, slices, Fixture::CHANNELS, Fixture::SAMPLE_RATE, bpm);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// No dropout, clipping, or silence the solo renders do not have. Onset
/// counts differ by the overlap and are judged by `assert_in_phase`. The
/// candidate ends where the stream ends: the capture budget's tail is silence
/// by construction, not a dropout.
async fn assert_continuous(label: &'static str, candidate: Vec<f32>, solo: Vec<f32>) {
    spawn_blocking(move || {
        let candidate = CochleaReport::measure(&candidate, Fixture::CHANNELS, Fixture::SAMPLE_RATE);
        let solo = CochleaReport::measure(&solo, Fixture::CHANNELS, Fixture::SAMPLE_RATE);
        let failures = time_stretch_failures(label, &candidate, &solo);
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    })
    .await
    .expect("continuity analysis task");
}

/// One track alone from `from_secs`, `frames` output frames at `rate`: the
/// reference a seam stream is judged against.
async fn solo_render(provider: Provider, from_secs: f64, frames: usize, rate: f32) -> Vec<f32> {
    let mut harness = ProductHarness::new_for_provider(Fixture::SOLO, provider, 0).await;
    harness.decks[0].set_rate(rate);
    if from_secs > 0.0 {
        harness.decks[0].seek(from_secs).expect("solo seek");
        harness.settle(Fixture::SOLO, 4).await;
    }
    let block = block_frames(&harness) as usize;
    harness.capture_frames(Fixture::SOLO, frames, block).await
}

/// Solo renders of the house and techno fixtures at `rates`, back to back.
async fn solo_pair(rates: [f32; 2]) -> Vec<f32> {
    let mut solo = Vec::new();
    for (names, rate) in [
        (Fixture::SOLO_HOUSE, rates[0]),
        (Fixture::SOLO_TECHNO, rates[1]),
    ] {
        let frames = (track_len(names[0]) as f64 / f64::from(rate)).round() as usize;
        solo.extend(solo_render(Provider::Rhythm(names), 0.0, frames, rate).await);
    }
    solo
}

/// Rate that plays a track authored at `original` bpm at `bpm`.
fn stretch_to(bpm: f64, original: f64) -> f32 {
    (bpm / original) as f32
}

#[kithara::test(tokio, timeout(Duration::from_secs(300)))]
async fn seam_rate_persists_into_next_track() {
    let mut harness = ProductHarness::new_for_provider(
        Fixture::SEAM_RATE_PERSISTS,
        Provider::Rhythm(Fixture::HOUSE_THEN_TECHNO),
        0,
    )
    .await;
    let block = block_frames(&harness);
    harness.play_all().await;
    harness.decks[0].set_rate(Fixture::RATE);
    let _pcm = harness
        .capture_frames(
            Fixture::SEAM_RATE_PERSISTS,
            total_frames(Fixture::SEAM_RATE_PERSISTS, Fixture::HOUSE_THEN_TECHNO),
            block as usize,
        )
        .await;
    let spans = spans(&usdt_trace::events());
    let rate_bits = u64::from(Fixture::RATE.to_bits());
    let events = usdt_trace::events();
    let smoothed: Vec<_> = events
        .iter()
        .filter(|event| event.probe == "rate_smoothed")
        .collect();
    let requested = smoothed
        .iter()
        .position(|event| event.field("target_bits") == Some(rate_bits))
        .expect("the deck receives the requested rate");
    assert!(
        smoothed[requested..]
            .iter()
            .all(|event| event.field("target_bits") == Some(rate_bits)),
        "no selection may retarget the deck away from {} after it was requested",
        Fixture::RATE
    );
    let settled = smoothed
        .iter()
        .position(|event| event.field("multiplier_bits") == Some(rate_bits))
        .expect("the owner smoother reaches the requested rate");
    assert!(
        smoothed[settled..]
            .iter()
            .all(|event| event.field("multiplier_bits") == Some(rate_bits)),
        "the queue seam must keep the deck multiplier at {}",
        Fixture::RATE
    );
    let ramp_deficit: f64 = smoothed[..settled]
        .iter()
        .filter(|event| event.field("target_bits") == Some(rate_bits))
        .map(|event| {
            let frames = event
                .field("frames")
                .expect("rate_smoothed names its frames") as f64;
            let multiplier = f32::from_bits(
                u32::try_from(event.field("multiplier_bits").expect("multiplier"))
                    .expect("f32 bits"),
            );
            frames * (1.0 - f64::from(multiplier) / f64::from(Fixture::RATE))
        })
        .sum();
    for (index, name) in Fixture::HOUSE_THEN_TECHNO.iter().enumerate() {
        let id: TrackId = harness.ids[0][index];
        let span = spans
            .get(&id.as_u64())
            .unwrap_or_else(|| panic!("track {index} `{name}` never rendered"));
        let len = track_len(name);
        assert_eq!(
            span.served, len,
            "track {index} `{name}` served every media frame"
        );
        let ramp = if index == 0 { ramp_deficit } else { 0.0 };
        let expected = (len as f64 / f64::from(Fixture::RATE) + ramp).round() as i64;
        let rendered = span.last - span.first;
        assert!(
            (rendered - expected).abs() <= block,
            "track {index} `{name}` spans {rendered} frames, produced {} audio frames, expected {expected} ± {block} at rate {}; source-free output frames={}",
            span.output_frames,
            Fixture::RATE,
            span.source_free_frames,
        );
    }
}

#[kithara::test(tokio, timeout(Duration::from_secs(300)))]
async fn seam_off_keeps_the_stream_continuous_at_original_tempo() {
    let mut harness = ProductHarness::new_for_provider(
        Fixture::SEAM_OFF,
        Provider::Rhythm(Fixture::HOUSE_THEN_TECHNO),
        0,
    )
    .await;
    let block = block_frames(&harness);
    let origin = i64::try_from(harness.rendered_frames).expect("origin fits i64");
    let pcm = harness
        .capture_frames(
            Fixture::SEAM_OFF,
            total_frames(Fixture::SEAM_OFF, Fixture::HOUSE_THEN_TECHNO),
            block as usize,
        )
        .await;
    let spans = spans(&usdt_trace::events());
    let b = spans[&harness.ids[0][1].as_u64()];
    let b_first = b.first - origin;
    let played = usize::try_from((b.last - origin) * i64::from(Fixture::CHANNELS))
        .expect("stream end fits usize");
    let fade = (f64::from(Fixture::FADE_SECS) * f64::from(Fixture::SAMPLE_RATE)).round() as i64;
    let (before, _) = seam_window(&pcm, b_first, fade, 2 * bar_frames(124.0));
    let (_, after) = seam_window(&pcm, b_first, fade, 2 * bar_frames(132.0));
    assert_continuous(
        "seam off",
        pcm[..played].to_vec(),
        solo_pair([stretch_to(124.0, 124.0), stretch_to(132.0, 132.0)]).await,
    )
    .await;
    assert_in_phase("seam off, house before the seam", &[&before], 124.0, true);
    assert_in_phase("seam off, techno after the fade", &[&after], 132.0, true);
    for (index, name) in Fixture::HOUSE_THEN_TECHNO.iter().enumerate() {
        let span = spans[&harness.ids[0][index].as_u64()];
        let len = i64::try_from(track_len(name)).expect("track length fits i64");
        let rendered = i64::try_from(span.output_frames).expect("rendered frames fit i64");
        assert!(
            (rendered - len).abs() <= block,
            "track {index} `{name}` rendered {rendered} frames at rate 1.0, expected {len} ± {block}",
        );
    }
}

#[kithara::test(tokio, timeout(Duration::from_secs(300)))]
async fn seam_honours_the_configured_crossfade_length() {
    let mut harness = ProductHarness::new_for_provider(
        Fixture::SEAM_FADE_LENGTH,
        Provider::Rhythm(Fixture::HOUSE_THEN_TECHNO),
        0,
    )
    .await;
    let block = block_frames(&harness);
    let _pcm = harness
        .capture_frames(
            Fixture::SEAM_FADE_LENGTH,
            total_frames(Fixture::SEAM_FADE_LENGTH, Fixture::HOUSE_THEN_TECHNO),
            block as usize,
        )
        .await;
    let spans = spans(&usdt_trace::events());
    let (a, b) = (
        spans[&harness.ids[0][0].as_u64()],
        spans[&harness.ids[0][1].as_u64()],
    );
    let overlap = a.last - b.first;
    let fade = (f64::from(Fixture::FADE_SECS) * f64::from(Fixture::SAMPLE_RATE)).round() as i64;
    assert!(
        (overlap - fade).abs() <= block,
        "tracks overlapped for {overlap} frames, configured crossfade is {fade} ± {block}",
    );
}

/// A synced deck that has played past the house lead-in, where the fixture
/// grid covers the presentation frontier.
async fn synced_house_deck() -> ProductHarness {
    let mut harness = ProductHarness::new_for_provider(
        Fixture::TRACK_GRID,
        Provider::Rhythm(Fixture::HOUSE_THEN_TECHNO),
        0,
    )
    .await;
    harness.set_tempo(Fixture::TRACK_GRID, 124.0, true).await;
    let lead_in = usize::try_from(bar_frames(124.0)).expect("lead-in fits usize");
    let _ = harness
        .capture_frames(Fixture::TRACK_GRID, lead_in, harness.block_frames)
        .await;
    harness.request_sync(Fixture::TRACK_GRID).await;
    assert!(
        harness.failures.is_empty(),
        "sync request failed: {:?}",
        harness.failures
    );
    harness
}

#[kithara::test(tokio, timeout(Duration::from_secs(300)))]
async fn a_complete_track_grid_is_prepared_on_the_synced_deck() {
    let mut harness = synced_house_deck().await;
    let grid = asset_grid(Fixture::HOUSE_ANALYSIS);
    let admission = harness
        .publish_track_grid(0, harness.ids[0][0], grid, BeatGridState::Complete)
        .await
        .unwrap_or_else(|error| panic!("publish the house grid: {error}"));
    assert!(
        matches!(admission, SyncAdmission::Prepared { .. }),
        "the deck prepares a warp map for a complete track grid, got {admission:?}"
    );
}

#[kithara::test(tokio, timeout(Duration::from_secs(300)))]
async fn published_track_grids_align_the_rendered_beats() {
    let case = DOWNTEMPO_HOUSE_SYNC;
    let grids = [
        asset_grid("rhythm_expected_analysis_downtempo_96_aligned"),
        asset_grid(Fixture::HOUSE_ANALYSIS),
    ];
    let mut tracks = Vec::new();
    let mut capture_origins = Vec::new();
    for audible in 0..case.decks() {
        let mut harness =
            ProductHarness::new_for_provider(case, DOWNTEMPO_HOUSE_PROVIDER, audible).await;
        let _ = harness
            .capture_frames(
                case,
                2 * Fixture::SAMPLE_RATE as usize,
                harness.block_frames,
            )
            .await;
        harness.request_sync(case).await;
        let mut activation = SessionFrame::new(0);
        for (deck, grid) in grids.iter().enumerate() {
            let admission = harness
                .publish_track_grid(
                    deck,
                    harness.ids[deck][0],
                    grid.clone(),
                    BeatGridState::Complete,
                )
                .await
                .expect("publish score grid");
            let SyncAdmission::Prepared {
                activation: prepared,
                ..
            } = admission
            else {
                panic!("covered score grid must prepare alignment: {admission:?}");
            };
            activation = activation.max(prepared);
        }
        let rendered = i64::try_from(harness.rendered_frames).unwrap_or(i64::MAX);
        let until_activation = i64::from(activation).saturating_sub(rendered).max(0);
        let settle_frames = usize::try_from(until_activation)
            .unwrap_or(usize::MAX)
            .saturating_add(usize::try_from(bar_frames(case.final_bpm())).unwrap_or(usize::MAX));
        let _ = harness
            .capture_frames(case, settle_frames, harness.block_frames)
            .await;
        capture_origins.push(harness.rendered_frames);
        tracks.push(
            harness
                .capture_frames(
                    case,
                    3 * Fixture::SAMPLE_RATE as usize,
                    harness.block_frames,
                )
                .await,
        );
        assert!(harness.failures.is_empty(), "{:?}", harness.failures);
    }
    let tracks = tracks.iter().map(Vec::as_slice).collect::<Vec<_>>();
    let failures = marked_synchronization_failures(
        "published score grids",
        &tracks,
        Fixture::CHANNELS,
        Fixture::SAMPLE_RATE,
        case.final_bpm(),
    );
    let mix = tracks[0]
        .iter()
        .zip(tracks[1])
        .map(|(left, right)| (left + right) * 0.5)
        .collect::<Vec<_>>();
    let artifact = write_audio_artifact(
        "published-track-grids-align-rendered-beats",
        Fixture::SAMPLE_RATE,
        Fixture::CHANNELS,
        &[
            ("deck-1", tracks[0]),
            ("deck-2", tracks[1]),
            ("mix", mix.as_slice()),
        ],
        &serde_json::json!({
            "oracle": "marked_synchronization_failures",
            "target_bpm": case.final_bpm(),
            "beat_phase_spread_frames": 0,
            "bar_phase_spread_beats": 0,
        }),
    )
    .expect("write oracle-checked sync audio artifact");
    if let Some(path) = artifact {
        eprintln!("KITHARA_AUDIO_ARTIFACT oracle: {}", path.display());
    }
    assert!(
        failures.is_empty(),
        "{}\ncapture_origins={capture_origins:?}",
        failures.join("\n"),
    );
}

#[kithara::test(tokio, timeout(Duration::from_secs(300)))]
async fn a_building_track_grid_defers_until_it_completes() {
    let mut harness = synced_house_deck().await;
    let item = harness.ids[0][0];
    let grid = asset_grid(Fixture::HOUSE_ANALYSIS);
    let building = harness
        .publish_track_grid(0, item, grid.clone(), BeatGridState::Building)
        .await
        .unwrap_or_else(|error| panic!("publish the building grid: {error}"));
    assert!(
        matches!(building, SyncAdmission::Deferred { .. }),
        "a building grid waits for coverage, got {building:?}"
    );
    let complete = harness
        .publish_track_grid(0, item, grid, BeatGridState::Complete)
        .await
        .unwrap_or_else(|error| panic!("republish the complete grid: {error}"));
    assert!(
        matches!(complete, SyncAdmission::Prepared { .. }),
        "the completed revision replaces the building one and is prepared, got {complete:?}"
    );
}

/// Every exact beat marker in `pcm[start..end]` (capture frames) lands on a
/// Host beat of the same span, and every Host beat at least one beat inside
/// the span carries a marker, so a track that drifts or falls silent fails.
fn host_locked_failures(
    harness: &ProductHarness,
    label: &str,
    pcm: &[f32],
    origin: u64,
    start: u64,
    end: u64,
) -> Vec<String> {
    let channels = usize::from(Fixture::CHANNELS);
    let at = |frame: u64| usize::try_from(frame - origin).expect("frame fits usize") * channels;
    let host_beats = harness
        .host_beats_in(start..end)
        .into_iter()
        .map(|frame| usize::try_from(frame - start).expect("beat fits usize"))
        .collect::<Vec<_>>();
    let slice = &pcm[at(start)..at(end)];
    let mut failures = host_beat_alignment_failures(
        label,
        slice,
        Fixture::CHANNELS,
        Fixture::SAMPLE_RATE,
        &host_beats,
    );
    let (markers, _) = marked_rhythm_markers(slice, Fixture::CHANNELS, Fixture::SAMPLE_RATE);
    let beat = usize::try_from(bar_frames(Fixture::HOST_BPM) / 4).expect("beat fits usize");
    let span = usize::try_from(end - start).expect("span fits usize");
    failures.extend(
        host_beats
            .iter()
            .filter(|frame| **frame >= beat && **frame + beat <= span)
            .filter(|frame| !markers.contains(frame))
            .map(|frame| format!("{label}: Host beat at frame {frame} carries no beat marker")),
    );
    failures
}

/// A synced deck playing a five-track queue in five tempos: every track, and
/// both tracks inside every crossfade, keep the Host tempo and beat phase.
#[kithara::test(tokio, timeout(Duration::from_secs(600)))]
async fn a_synced_queue_holds_every_track_on_the_host_grid_through_crossfades() {
    let case = Fixture::FIVE_TRACK_QUEUE;
    let mut harness =
        ProductHarness::new_for_provider(case, Provider::Rhythm(Fixture::FIVE_TRACK_NAMES), 0)
            .await;
    harness.request_sync(case).await;
    for (index, (wav, analysis, _)) in Fixture::FIVE_TEMPOS.iter().enumerate() {
        let admission = harness
            .publish_track_grid(
                0,
                harness.ids[0][index],
                asset_grid(analysis),
                BeatGridState::Complete,
            )
            .await
            .unwrap_or_else(|error| panic!("publish `{wav}` grid: {error}"));
        assert!(
            !matches!(admission, SyncAdmission::Unavailable { .. }),
            "`{wav}` grid is admitted, got {admission:?}"
        );
    }
    harness.play_all().await;
    let origin = harness.rendered_frames;
    let frames: f64 = Fixture::FIVE_TEMPOS
        .iter()
        .map(|(wav, _, bpm)| track_len(wav) as f64 * bpm / Fixture::HOST_BPM)
        .sum();
    let pcm = harness
        .capture_frames(
            case,
            frames.ceil() as usize + 4 * Fixture::SAMPLE_RATE as usize,
            harness.block_frames,
        )
        .await;
    assert!(harness.failures.is_empty(), "{:?}", harness.failures);
    let spans = spans(&usdt_trace::events());
    let capture = |session: i64| {
        u64::try_from(session - harness.session_frame(0)).expect("span on capture axis")
    };
    let bar = u64::try_from(bar_frames(Fixture::HOST_BPM)).expect("bar fits u64");
    let mut failures = Vec::new();
    let mut previous: Option<TrackSpan> = None;
    for (index, (wav, _, bpm)) in Fixture::FIVE_TEMPOS.iter().enumerate() {
        let span = *spans
            .get(&harness.ids[0][index].as_u64())
            .unwrap_or_else(|| panic!("track {index} `{wav}` never rendered"));
        let (first, last) = (capture(span.first).max(origin), capture(span.last));
        assert!(
            index == 0 || capture(span.first) >= origin,
            "track {index} `{wav}` rendered before the capture"
        );
        assert!(
            first + 2 * bar < last,
            "track {index} `{wav}` has no body between its seams"
        );
        let body = (first + bar, last - bar);
        failures.extend(host_locked_failures(
            &harness,
            &format!("track {index} `{wav}` ({bpm} bpm) body"),
            &pcm,
            origin,
            body.0,
            body.1,
        ));
        if let Some(previous) = previous {
            let (fade_start, fade_end) = (first, capture(previous.last));
            assert!(
                fade_start < fade_end,
                "track {index} `{wav}` does not crossfade into its predecessor"
            );
            failures.extend(host_locked_failures(
                &harness,
                &format!("crossfade into track {index} `{wav}`"),
                &pcm,
                origin,
                fade_start,
                fade_end,
            ));
        }
        previous = Some(span);
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
