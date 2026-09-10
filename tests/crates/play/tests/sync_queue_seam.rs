#![cfg(not(target_arch = "wasm32"))]

//! Seam contract of a deck playing a queue: the configured crossfade length is
//! honoured, deck-level timestretch persists into the next track, and without a
//! grid the stream stays continuous at the original tempo.

use std::{collections::BTreeMap, num::NonZeroU32};

use kithara::{
    analysis::{AnalysisFile, AnalysisFingerprint, BeatArtifact},
    events::TrackId,
    platform::{time::Duration, tokio::task::spawn_blocking},
    warp::{AssetAxis, BeatGridState, SyncAdmission},
};
use kithara_integration_tests::{
    cochlea::{
        CochleaReport, marked_synchronization_failures, synchronization_failures,
        time_stretch_failures,
    },
    grid::segment_set,
    kithara,
};
use kithara_test_fixtures::{asset::Asset, assets};
use kithara_test_utils::probe::{IntoProbeArg, capture as probe_capture, capture::Recorder};

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
        SyncCase::queued("seam_rate_persists", Self::SAMPLE_RATE, 2, Self::FADE_SECS);
    const SEAM_OFF: SyncCase = SyncCase::queued("seam_off", Self::SAMPLE_RATE, 2, Self::FADE_SECS);
    const SEAM_FADE_LENGTH: SyncCase =
        SyncCase::queued("seam_fade_length", Self::SAMPLE_RATE, 2, Self::FADE_SECS);
    const TRACK_GRID: SyncCase =
        SyncCase::queued("track_grid", Self::SAMPLE_RATE, 2, Self::FADE_SECS);
    const HOUSE_ANALYSIS: &str = "rhythm_expected_analysis_house_124_aligned";
    const ANALYSIS_FINGERPRINT: &str = "rhythm-fixture:v1";
}

/// The asset grid the fixture score promises for `wav`, read from its
/// analysis sidecar and laid on the WAV's own frame axis.
fn asset_grid(wav: &str, analysis: &str) -> kithara::warp::SegmentSet {
    let asset = assets::by_name(analysis).unwrap_or_else(|| panic!("missing `{analysis}`"));
    let fingerprint = AnalysisFingerprint::new(Some(Fixture::ANALYSIS_FINGERPRINT), None);
    let artifact: BeatArtifact = AnalysisFile::parse(asset.bytes(), &fingerprint)
        .unwrap_or_else(|error| panic!("decode `{analysis}`: {error}"))
        .latest()
        .analysis()
        .beat()
        .unwrap_or_else(|| panic!("`{analysis}` has no beat analysis"))
        .artifact()
        .clone();
    let rate = NonZeroU32::new(Fixture::SAMPLE_RATE).expect("fixture sample rate");
    segment_set(&artifact, AssetAxis::new(rate, track_len(wav)))
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

pub(super) fn spans(recorder: &Recorder) -> BTreeMap<u64, TrackSpan> {
    let mut spans = BTreeMap::new();
    for event in recorder.events_with_probe("render") {
        let Some(base) = event.u64("output_base").filter(|base| *base != u64::MAX) else {
            continue;
        };
        let Some(frames) = event.u64("rendered_frames").filter(|frames| *frames > 0) else {
            continue;
        };
        let track = event.u64("track_id").expect("render names its track");
        let offset = event.u64("range_start").expect("render names its offset");
        let first = i64::from_probe_arg(base) + i64::from_probe_arg(offset);
        let last = first + i64::try_from(frames).expect("render frame count fits i64");
        let served = event
            .u64("served_media_frames")
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

#[ignore = "ignored-red: the leading track is cut at the queue tick and the deck rate is not carried into the next track (plan 1 Tasks 4-5), 2026-09-07"]
#[kithara::test(tokio, timeout(Duration::from_secs(300)))]
async fn seam_rate_persists_into_next_track() {
    let recorder = probe_capture::install();
    let mut harness = ProductHarness::new_for_provider(
        Fixture::SEAM_RATE_PERSISTS,
        Provider::Rhythm(Fixture::HOUSE_THEN_TECHNO),
        0,
    )
    .await;
    let block = block_frames(&harness);
    harness.decks[0].set_rate(Fixture::RATE);
    let _pcm = harness
        .capture_frames(
            Fixture::SEAM_RATE_PERSISTS,
            total_frames(Fixture::SEAM_RATE_PERSISTS, Fixture::HOUSE_THEN_TECHNO),
            block as usize,
        )
        .await;
    let spans = spans(&recorder);
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
        let expected = (len as f64 / f64::from(Fixture::RATE)).round() as i64;
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
    let recorder = probe_capture::install();
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
    let spans = spans(&recorder);
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
    let recorder = probe_capture::install();
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
    let spans = spans(&recorder);
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
    let harness = synced_house_deck().await;
    let grid = asset_grid(Fixture::RHYTHM_HOUSE_124, Fixture::HOUSE_ANALYSIS);
    let admission = harness
        .publish_track_grid(0, harness.ids[0][0], grid, BeatGridState::Complete)
        .await
        .unwrap_or_else(|error| panic!("publish the house grid: {error}"));
    assert!(
        matches!(admission, SyncAdmission::Prepared { .. }),
        "the deck prepares a warp map for a complete track grid, got {admission:?}"
    );
}

#[ignore = "ignored-red: prepared alignment is not yet delivered to the renderer as a versioned source/output map with exact-frame activation (next Warp phase), 2026-09-10"]
#[kithara::test(tokio, timeout(Duration::from_secs(300)))]
async fn published_track_grids_align_the_rendered_beats() {
    let case = DOWNTEMPO_HOUSE_SYNC;
    let grids = [
        asset_grid(
            "rhythm_wav_downtempo_96_aligned",
            "rhythm_expected_analysis_downtempo_96_aligned",
        ),
        asset_grid(Fixture::RHYTHM_HOUSE_124, Fixture::HOUSE_ANALYSIS),
    ];
    let mut tracks = Vec::new();
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
            assert!(
                matches!(admission, SyncAdmission::Prepared { .. }),
                "covered score grid must prepare alignment: {admission:?}"
            );
        }
        let _ = harness
            .capture_frames(case, Fixture::SAMPLE_RATE as usize, harness.block_frames)
            .await;
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
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[kithara::test(tokio, timeout(Duration::from_secs(300)))]
async fn a_building_track_grid_defers_until_it_completes() {
    let harness = synced_house_deck().await;
    let item = harness.ids[0][0];
    let grid = asset_grid(Fixture::RHYTHM_HOUSE_124, Fixture::HOUSE_ANALYSIS);
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
