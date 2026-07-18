#![cfg(not(target_arch = "wasm32"))]

use std::{fs, iter::from_fn, mem::size_of, num::NonZeroU32, path::Path};

use kithara::{
    assets::StoreOptions,
    audio::{BeatGrid, ReadOutcome, TrackBeat, analysis::TrackAnalysis},
    events::{AudioEvent, Event, EventBus},
    platform::{
        sync::Arc,
        time::{Duration, sleep},
    },
    play::{
        PlayError, PlaybackDirection, Resource, ResourceBlueprint, ResourceConfig,
        SelectTransition, SessionBeat, SessionError, SessionTrackControl, SessionTransportSnapshot,
        Tempo, TrackBinding,
    },
};
use kithara_integration_tests::{
    SignalFormat, SignalSpec, SignalSpecLength, TestServerHelper, TestTempDir, kithara,
    offline::OfflineSession, temp_dir, wav::create_wav_header,
};
use num_traits::ToPrimitive;

use super::offline_player_harness::{OfflinePlayerHarness, OfflinePlayerOptions};

const BLOCK_FRAMES: usize = 512;
const CHANNELS: u16 = 2;
const FIXTURE_FRAMES: usize = 176_400;
const SAMPLE_RATE: u32 = 44_100;
const SESSION_TEMPO: f64 = 120.0;
const TRACK_TEMPO: f64 = 100.0;

fn binding(session_anchor: SessionBeat) -> TrackBinding {
    let sample_rate = NonZeroU32::new(SAMPLE_RATE).expect("static sample rate");
    let frames_per_beat = (60.0 * f64::from(SAMPLE_RATE) / TRACK_TEMPO)
        .round()
        .to_u64()
        .expect("track tempo resolves to a frame span");
    let markers = (0..=6).map(|beat| beat * frames_per_beat).collect();
    let analysis = TrackAnalysis::with_source_rate(
        Some(BeatGrid::new(TRACK_TEMPO, markers, vec![0], Vec::new())),
        None,
        u64::try_from(FIXTURE_FRAMES).expect("fixture frame count fits u64"),
        sample_rate,
    );
    TrackBinding::new(
        &analysis,
        sample_rate,
        session_anchor,
        TrackBeat::new(0.0).expect("finite track anchor"),
        PlaybackDirection::Forward,
    )
    .expect("fixture analysis can be bound")
}

fn exact_binding(session_anchor: SessionBeat, source_tempo: u32) -> TrackBinding {
    let sample_rate = NonZeroU32::new(SAMPLE_RATE).expect("static sample rate");
    let frames_per_minute = 60 * u64::from(SAMPLE_RATE);
    assert_eq!(frames_per_minute % u64::from(source_tempo), 0);
    let frames_per_beat = frames_per_minute / u64::from(source_tempo);
    let markers = (0..=6).map(|beat| beat * frames_per_beat).collect();
    let analysis = TrackAnalysis::with_source_rate(
        Some(BeatGrid::new(
            f64::from(source_tempo),
            markers,
            vec![0],
            Vec::new(),
        )),
        None,
        u64::try_from(FIXTURE_FRAMES).expect("fixture frame count fits u64"),
        sample_rate,
    );
    TrackBinding::new(
        &analysis,
        sample_rate,
        session_anchor,
        TrackBeat::new(0.0).expect("finite track anchor"),
        PlaybackDirection::Forward,
    )
    .expect("exact source grid can be bound")
}

async fn insert_bound_sine(
    harness: &OfflinePlayerHarness,
    server: &TestServerHelper,
    temp_dir: &TestTempDir,
    source_tempo: u32,
    frequency: f64,
    session_anchor: SessionBeat,
) -> usize {
    let resource = bound_sine_resource(harness, server, temp_dir, frequency).await;
    let index = harness.player().item_count();
    harness
        .player()
        .insert_with_binding(
            resource,
            None,
            exact_binding(session_anchor, source_tempo),
            None,
        )
        .await
        .expect("insert prepared bound fixture");
    index
}

async fn bound_sine_resource(
    harness: &OfflinePlayerHarness,
    server: &TestServerHelper,
    temp_dir: &TestTempDir,
    frequency: f64,
) -> Resource {
    let spec = SignalSpec {
        format: SignalFormat::Wav,
        length: SignalSpecLength::Frames(FIXTURE_FRAMES),
        channels: CHANNELS,
        sample_rate: SAMPLE_RATE,
        bit_rate: None,
    };
    let url = server.sine(&spec, frequency).await;
    let config = ResourceConfig::for_src(url.as_str())
        .expect("valid fixture URL")
        .store(StoreOptions::new(temp_dir.path()))
        .byte_pool(harness.player().byte_pool().clone())
        .pcm_pool(harness.player().pcm_pool().clone())
        .build();
    let mut resource = Resource::new(harness.player().prepare_config(config))
        .await
        .expect("open exact-tempo WAV resource");
    resource
        .preload()
        .await
        .expect("preload exact-tempo WAV resource");
    resource
}

async fn prepare_bound_players(
    server: &TestServerHelper,
    temp_dir: &TestTempDir,
    source_tempos: &[u32],
) -> Vec<OfflinePlayerHarness> {
    let session = Arc::new(OfflineSession::new_manual());
    let harnesses: Vec<_> = source_tempos
        .iter()
        .map(|_| {
            OfflinePlayerHarness::in_session(
                OfflinePlayerOptions::builder()
                    .crossfade_duration(0.0)
                    .build(),
                SAMPLE_RATE,
                Arc::clone(&session),
            )
        })
        .collect();
    let volume = 1.0 / source_tempos.len().to_f32().expect("player count fits f32");

    for harness in &harnesses {
        harness
            .player()
            .ensure_engine_started()
            .expect("offline engine starts");
        harness
            .player()
            .ensure_slot()
            .expect("offline player slot is allocated");
        harness.player().set_volume(volume);
    }
    harnesses[0]
        .set_session_tempo(Tempo::new(SESSION_TEMPO).expect("valid initial tempo"))
        .expect("initial tempo accepted");
    let _ = harnesses[0].render(1);
    let initial = harnesses[0]
        .session_transport()
        .expect("initial tempo committed");

    for (index, source_tempo) in source_tempos.iter().copied().enumerate() {
        let item = insert_bound_sine(
            &harnesses[index],
            server,
            temp_dir,
            source_tempo,
            220.0 + index.to_f64().expect("player index fits f64") * 110.0,
            initial.position(),
        )
        .await;
        harnesses[index]
            .player()
            .select_item(item, true)
            .expect("select prepared bound fixture");
    }
    for _ in 0..16 {
        let _ = harnesses[0].render(BLOCK_FRAMES);
        for harness in &harnesses {
            harness.tick_and_drain();
        }
        sleep(Duration::from_millis(1)).await;
    }
    assert!(
        harnesses
            .iter()
            .all(|harness| harness.player().is_playing())
    );
    harnesses
}

fn commit_tempo_change(
    harnesses: &[OfflinePlayerHarness],
    tempo: Tempo,
) -> (SessionTransportSnapshot, SessionTransportSnapshot) {
    let before = harnesses[0]
        .session_transport()
        .expect("transport before tempo change");
    let peers: Vec<_> = harnesses[1..]
        .iter()
        .map(|harness| harness.player().as_ref())
        .collect();
    harnesses[0]
        .player()
        .set_session_tempo(&peers, tempo)
        .expect("multi-player tempo change accepted");

    let old_slope = before.tempo().beats_per_minute() / (f64::from(SAMPLE_RATE) * 60.0);
    let new_slope = tempo.beats_per_minute() / (f64::from(SAMPLE_RATE) * 60.0);
    let tolerance = old_slope.max(new_slope);
    let mut previous = before;
    let after = (0..BLOCK_FRAMES * 8)
        .find_map(|_| {
            let _ = harnesses[0].render(1);
            harnesses[0]
                .tick_session()
                .expect("tempo transaction progresses");
            let snapshot = harnesses[0]
                .session_transport()
                .expect("transport snapshot");
            let slope = if snapshot.revision() == before.revision() {
                old_slope
            } else {
                new_slope
            };
            assert!(
                (snapshot.position().get() - previous.position().get() - slope).abs() <= tolerance
            );
            previous = snapshot;
            (snapshot.revision() != before.revision()).then_some(snapshot)
        })
        .expect("tempo change commits within the render budget");

    assert_eq!(after.revision(), before.revision() + 1);
    assert_eq!(after.tempo(), tempo);
    for harness in harnesses {
        assert_eq!(harness.session_transport().expect("shared snapshot"), after);
    }
    (before, after)
}

fn assert_player_phase_aligned(harnesses: &[OfflinePlayerHarness]) {
    let mut positions = harnesses.iter().map(|harness| {
        harness
            .player()
            .playback_snapshot()
            .expect("player phase snapshot")
            .position()
    });
    let first = positions.next().expect("at least one player");
    let tolerance = 1.0 / f64::from(SAMPLE_RATE);
    assert!(positions.all(|position| (position - first).abs() <= tolerance));
}

fn write_rendered_wav(path: &Path, rendered: &[f32]) {
    let data_bytes = rendered.len() * size_of::<i16>();
    let mut wav = create_wav_header(SAMPLE_RATE, CHANNELS, Some(data_bytes));
    wav.reserve(data_bytes);
    for sample in rendered {
        let quantized = (sample.clamp(-1.0, 1.0) * f32::from(i16::MAX))
            .round()
            .to_i16()
            .expect("clamped sample fits i16");
        wav.extend_from_slice(&quantized.to_le_bytes());
    }
    fs::write(path, wav).expect("write offline elastic artifact");
}

fn positive_crossing_frequency(rendered: &[f32]) -> Option<f64> {
    let mut previous = 0.0_f32;
    let mut started = false;
    let mut first = None;
    let mut last = 0usize;
    let mut crossings = 0usize;
    for (frame, channels) in rendered.chunks_exact(usize::from(CHANNELS)).enumerate() {
        let sample = channels[0];
        if !started {
            if sample.abs() < 0.05 {
                continue;
            }
            started = true;
            previous = sample;
            continue;
        }
        if previous <= 0.0 && sample > 0.0 {
            first.get_or_insert(frame);
            last = frame;
            crossings += 1;
        }
        previous = sample;
    }
    let first = first?;
    let intervals = crossings.checked_sub(1)?.to_f64()?;
    let span = last.checked_sub(first)?.to_f64()?;
    (span > 0.0).then_some(intervals * f64::from(SAMPLE_RATE) / span)
}

#[kithara::test(tokio, flash(false))]
async fn two_bound_players_commit_one_tempo_revision(temp_dir: TestTempDir) {
    let server = TestServerHelper::new().await;
    let harnesses = prepare_bound_players(&server, &temp_dir, &[100, 120]).await;
    let (before, after) =
        commit_tempo_change(&harnesses, Tempo::new(100.0).expect("valid changed tempo"));
    assert_eq!(before.tempo().beats_per_minute(), SESSION_TEMPO);
    assert_eq!(after.tempo().beats_per_minute(), 100.0);

    let mut rendered = Vec::new();
    for _ in 0..8 {
        rendered.extend(harnesses[0].render(BLOCK_FRAMES));
        for harness in &harnesses {
            harness.tick_and_drain();
        }
        sleep(Duration::from_millis(1)).await;
    }
    assert!(rendered.iter().any(|sample| sample.abs() > 1.0e-4));
    assert!(
        harnesses
            .iter()
            .all(|harness| harness.player().is_playing())
    );
    assert_player_phase_aligned(&harnesses);
}

#[kithara::test(tokio, flash(false))]
async fn two_bound_players_commit_one_session_seek_revision(temp_dir: TestTempDir) {
    let server = TestServerHelper::new().await;
    let harnesses = prepare_bound_players(&server, &temp_dir, &[100, 120]).await;
    let before = harnesses[0]
        .session_transport()
        .expect("transport before session seek");
    let target = SessionBeat::new(2.0).expect("finite seek target");
    let players: Vec<_> = harnesses
        .iter()
        .map(|harness| Arc::clone(harness.player()))
        .collect();
    let seek = kithara::platform::tokio::task::spawn(async move {
        let peers: Vec<_> = players[1..].iter().map(AsRef::as_ref).collect();
        players[0].seek_session(&peers, target).await
    });

    for _ in 0..512 {
        if seek.is_finished() {
            break;
        }
        let _ = harnesses[0].render(1);
        for harness in &harnesses {
            harness.tick_and_drain();
        }
        sleep(Duration::from_millis(10)).await;
    }
    assert!(seek.is_finished(), "session seek preparation stays bounded");
    seek.await
        .expect("session seek task joins")
        .expect("session seek preparation commits");

    let after = (0..BLOCK_FRAMES * 8)
        .find_map(|_| {
            let _ = harnesses[0].render(1);
            harnesses[0]
                .tick_session()
                .expect("session seek transaction progresses");
            let snapshot = harnesses[0]
                .session_transport()
                .expect("transport after session seek render");
            (snapshot.revision() != before.revision()).then_some(snapshot)
        })
        .expect("session seek commits within the render budget");

    let one_frame = after.tempo().beats_per_minute() / (f64::from(SAMPLE_RATE) * 60.0);
    assert_eq!(after.revision(), before.revision() + 1);
    assert!((after.position().get() - target.get() - one_frame).abs() <= one_frame);
    for harness in &harnesses {
        assert_eq!(harness.session_transport().expect("shared snapshot"), after);
    }
    assert_player_phase_aligned(&harnesses);
}

#[kithara::test(tokio, flash(false))]
async fn failed_session_seek_leaves_the_transport_unchanged(temp_dir: TestTempDir) {
    let server = TestServerHelper::new().await;
    let harnesses = prepare_bound_players(&server, &temp_dir, &[100, 120]).await;
    let before = harnesses[0]
        .session_transport()
        .expect("transport before rejected session seek");
    let peers: Vec<_> = harnesses[1..]
        .iter()
        .map(|harness| harness.player().as_ref())
        .collect();
    let target = SessionBeat::new(7.0).expect("finite out-of-domain target");

    let error = harnesses[0]
        .player()
        .seek_session(&peers, target)
        .await
        .expect_err("out-of-domain seek is rejected");

    assert!(matches!(error, PlayError::ElasticPreparation { .. }));
    for harness in &harnesses {
        assert_eq!(
            harness
                .session_transport()
                .expect("transport after rejected session seek"),
            before
        );
        assert!(harness.player().is_playing());
    }
}

#[kithara::test(tokio, flash(false))]
async fn stale_session_seek_is_cancelled_before_a_retry(temp_dir: TestTempDir) {
    let server = TestServerHelper::new().await;
    let harnesses = prepare_bound_players(&server, &temp_dir, &[100, 120]).await;
    let target = SessionBeat::new(2.1).expect("finite seek target");
    let players: Vec<_> = harnesses
        .iter()
        .map(|harness| Arc::clone(harness.player()))
        .collect();
    let stale_seek = kithara::platform::tokio::task::spawn(async move {
        let peers: Vec<_> = players[1..].iter().map(AsRef::as_ref).collect();
        players[0].seek_session(&peers, target).await
    });
    sleep(Duration::from_millis(10)).await;

    let (_, tempo_after) =
        commit_tempo_change(&harnesses, Tempo::new(100.0).expect("valid changed tempo"));
    let error = stale_seek
        .await
        .expect("stale seek task joins")
        .expect_err("tempo commit invalidates the staged seek");
    assert!(matches!(error, PlayError::SessionSeekPreparationFailed));
    for harness in &harnesses {
        let position = harness
            .player()
            .playback_snapshot()
            .expect("position after stale seek")
            .position();
        assert!(
            position < 0.5,
            "stale seek moved source position to {position}"
        );
    }

    let _ = harnesses[0].render(1);
    for harness in &harnesses {
        harness.tick_and_drain();
    }

    let players: Vec<_> = harnesses
        .iter()
        .map(|harness| Arc::clone(harness.player()))
        .collect();
    let retry = kithara::platform::tokio::task::spawn(async move {
        let peers: Vec<_> = players[1..].iter().map(AsRef::as_ref).collect();
        players[0].seek_session(&peers, target).await
    });
    for _ in 0..512 {
        if retry.is_finished() {
            break;
        }
        let _ = harnesses[0].render(1);
        for harness in &harnesses {
            harness.tick_and_drain();
        }
        sleep(Duration::from_millis(10)).await;
    }
    retry
        .await
        .expect("retry task joins")
        .expect("retry commits after cancellation");

    let after = (0..BLOCK_FRAMES * 8)
        .find_map(|_| {
            let _ = harnesses[0].render(1);
            harnesses[0]
                .tick_session()
                .expect("retry transaction progresses");
            let snapshot = harnesses[0]
                .session_transport()
                .expect("transport after retry");
            (snapshot.revision() != tempo_after.revision()).then_some(snapshot)
        })
        .expect("retry commits within the render budget");
    assert_eq!(after.revision(), tempo_after.revision() + 1);
    assert!((after.position().get() - target.get()).abs() < 0.01);
}

#[kithara::test(tokio, flash(false))]
async fn explicit_join_is_silent_before_its_beat_and_uses_the_existing_transport(
    temp_dir: TestTempDir,
) {
    let server = TestServerHelper::new().await;
    let session = Arc::new(OfflineSession::new_manual());
    let harnesses: Vec<_> = (0..2)
        .map(|_| {
            OfflinePlayerHarness::in_session(
                OfflinePlayerOptions::builder()
                    .crossfade_duration(0.0)
                    .build(),
                SAMPLE_RATE,
                Arc::clone(&session),
            )
        })
        .collect();
    for harness in &harnesses {
        harness
            .player()
            .ensure_engine_started()
            .expect("offline engine starts");
        harness
            .player()
            .ensure_slot()
            .expect("offline player slot is allocated");
    }
    harnesses[0]
        .set_session_tempo(Tempo::new(SESSION_TEMPO).expect("valid initial tempo"))
        .expect("initial tempo accepted");
    let _ = harnesses[0].render(1);
    let initial = harnesses[0]
        .session_transport()
        .expect("initial tempo committed");
    let active = insert_bound_sine(
        &harnesses[0],
        &server,
        &temp_dir,
        100,
        220.0,
        initial.position(),
    )
    .await;
    harnesses[0]
        .player()
        .select_item(active, true)
        .expect("active bound fixture selected");
    harnesses[0].player().set_volume(0.0);
    harnesses[1].player().set_volume(1.0);
    for _ in 0..8 {
        let _ = harnesses[0].render(BLOCK_FRAMES);
        for harness in &harnesses {
            harness.tick_and_drain();
        }
        sleep(Duration::from_millis(1)).await;
    }

    let before = harnesses[0]
        .session_transport()
        .expect("transport before explicit join");
    let boundary_offset = 37usize;
    let lead_frames = BLOCK_FRAMES * 2 + boundary_offset;
    let slope = before.tempo().beats_per_minute() / (f64::from(SAMPLE_RATE) * 60.0);
    let target = SessionBeat::new(
        before.position().get() + lead_frames.to_f64().expect("lead frame count fits f64") * slope,
    )
    .expect("future join beat is finite");
    let joining_binding = exact_binding(initial.position(), 120);
    assert!(
        joining_binding
            .source_frame_at(target)
            .expect("join target resolves against binding")
            .is_some()
    );
    let resource = bound_sine_resource(&harnesses[1], &server, &temp_dir, 440.0).await;

    harnesses[1]
        .player()
        .join_track_at(resource, None, joining_binding, target)
        .await
        .expect("explicit join is prepared and scheduled");
    assert_eq!(
        harnesses[0]
            .session_transport()
            .expect("transport remains observable after join scheduling")
            .revision(),
        before.revision()
    );

    for _ in 0..2 {
        let rendered = harnesses[0].render(BLOCK_FRAMES);
        assert!(rendered.iter().all(|sample| sample.abs() <= 1.0e-4));
        for harness in &harnesses {
            harness.tick_and_drain();
        }
    }
    let boundary = harnesses[0].render(BLOCK_FRAMES);
    let silent_samples = boundary_offset * usize::from(CHANNELS);
    assert!(
        boundary[..silent_samples]
            .iter()
            .all(|sample| sample.abs() <= 1.0e-4)
    );
    assert!(
        boundary[silent_samples..]
            .iter()
            .any(|sample| sample.abs() > 0.05)
    );
    for harness in &harnesses {
        harness.tick_and_drain();
    }
    let after = harnesses[0]
        .session_transport()
        .expect("transport after explicit join");
    assert_eq!(after.revision(), before.revision());
    assert_eq!(after.tempo(), before.tempo());
    let frequency = positive_crossing_frequency(&boundary[silent_samples..])
        .expect("joined output contains a measurable waveform");
    assert!(
        (frequency - 440.0).abs() < 10.0,
        "frequency was {frequency}"
    );
}

#[kithara::test(tokio, flash(false))]
async fn four_bound_players_write_one_tempo_revision(temp_dir: TestTempDir) {
    let server = TestServerHelper::new().await;
    let harnesses = prepare_bound_players(&server, &temp_dir, &[100, 105, 120, 140]).await;
    let (_, after) =
        commit_tempo_change(&harnesses, Tempo::new(100.0).expect("valid changed tempo"));

    let mut rendered = Vec::new();
    for _ in 0..8 {
        rendered.extend(harnesses[0].render(BLOCK_FRAMES));
        for harness in &harnesses {
            harness.tick_and_drain();
        }
        sleep(Duration::from_millis(1)).await;
    }
    assert_eq!(after.tempo().beats_per_minute(), 100.0);
    assert!(rendered.iter().any(|sample| sample.abs() > 1.0e-4));
    assert!(
        harnesses
            .iter()
            .all(|harness| harness.player().is_playing())
    );
    assert_player_phase_aligned(&harnesses);

    let artifact = temp_dir.path().join("four-track-tempo-transaction.wav");
    write_rendered_wav(&artifact, &rendered);
    assert!(
        fs::metadata(&artifact)
            .expect("four-track offline artifact metadata")
            .len()
            > 44
    );
}

#[kithara::test(tokio, flash(false))]
async fn unsupported_peer_rejects_tempo_before_session_mutation(temp_dir: TestTempDir) {
    let server = TestServerHelper::new().await;
    let harnesses = prepare_bound_players(&server, &temp_dir, &[100, 108]).await;
    let before = harnesses[0]
        .session_transport()
        .expect("transport before rejected tempo");

    let error = harnesses[0]
        .player()
        .set_session_tempo(
            &[harnesses[1].player().as_ref()],
            Tempo::new(70.0).expect("valid rejected tempo"),
        )
        .expect_err("unsupported peer rejects the whole tempo change");

    assert!(matches!(error, PlayError::SessionTempoUnsupported { .. }));
    assert_eq!(
        harnesses[0]
            .session_transport()
            .expect("transport after rejection"),
        before
    );
    let mut rendered = Vec::new();
    for _ in 0..4 {
        rendered.extend(harnesses[0].render(BLOCK_FRAMES));
        for harness in &harnesses {
            harness.tick_and_drain();
        }
        sleep(Duration::from_millis(1)).await;
    }
    assert!(rendered.iter().any(|sample| sample.abs() > 1.0e-4));
    assert!(
        harnesses
            .iter()
            .all(|harness| harness.player().is_playing())
    );
}

#[kithara::test(tokio, flash(false))]
async fn paused_unsupported_peer_rejects_tempo_before_session_mutation(temp_dir: TestTempDir) {
    let server = TestServerHelper::new().await;
    let harnesses = prepare_bound_players(&server, &temp_dir, &[100, 108]).await;
    harnesses[1].player().pause();
    let _ = harnesses[0].render(1);
    for harness in &harnesses {
        harness.tick_and_drain();
    }
    assert!(!harnesses[1].player().is_playing());
    let before = harnesses[0]
        .session_transport()
        .expect("transport before rejected paused peer");

    let error = harnesses[0]
        .player()
        .set_session_tempo(
            &[harnesses[1].player().as_ref()],
            Tempo::new(70.0).expect("valid rejected tempo"),
        )
        .expect_err("unsupported paused peer rejects the whole tempo change");

    assert!(matches!(error, PlayError::SessionTempoUnsupported { .. }));
    assert_eq!(
        harnesses[0]
            .session_transport()
            .expect("transport after rejected paused peer"),
        before
    );
}

#[kithara::test(tokio, flash(false))]
async fn local_handover_rejects_tempo_before_session_mutation(temp_dir: TestTempDir) {
    let server = TestServerHelper::new().await;
    let harnesses = prepare_bound_players(&server, &temp_dir, &[100]).await;
    let harness = &harnesses[0];
    let anchor = harness
        .session_transport()
        .expect("transport before local handover");
    let next = insert_bound_sine(harness, &server, &temp_dir, 108, 330.0, anchor.position()).await;
    harness
        .player()
        .select_item_with_crossfade(
            next,
            SelectTransition {
                autoplay: true,
                crossfade_seconds: 1.0,
            },
        )
        .expect("start local bound-track handover");
    let _ = harness.render(BLOCK_FRAMES);
    harness.tick_and_drain();
    assert!(
        harness
            .player()
            .playback_snapshot()
            .expect("handover playback snapshot")
            .has_multiple_tracks()
    );
    let before = harness
        .session_transport()
        .expect("transport during local handover");

    let error = harness
        .player()
        .set_session_tempo(&[], Tempo::new(100.0).expect("valid changed tempo"))
        .expect_err("local handover rejects a session tempo change");

    assert!(matches!(error, PlayError::SessionTempoHandoverActive));
    assert_eq!(
        harness
            .session_transport()
            .expect("transport after rejected local handover"),
        before
    );
}

#[kithara::test(tokio, flash(false))]
async fn omitted_peer_rejects_tempo_before_session_mutation(temp_dir: TestTempDir) {
    let server = TestServerHelper::new().await;
    let harnesses = prepare_bound_players(&server, &temp_dir, &[100, 120]).await;
    let before = harnesses[0]
        .session_transport()
        .expect("transport before incomplete tempo request");

    let error = harnesses[0]
        .player()
        .set_session_tempo(&[], Tempo::new(100.0).expect("valid changed tempo"))
        .expect_err("omitting an active peer rejects the tempo change");

    assert!(matches!(
        error,
        PlayError::Session(SessionError::TransportPlayersChanged { .. })
    ));
    assert_eq!(
        harnesses[0]
            .session_transport()
            .expect("transport after incomplete request"),
        before
    );
}

#[kithara::test(tokio, flash(false))]
async fn bound_track_renders_elastic_audio_to_an_offline_wav(temp_dir: TestTempDir) {
    let server = TestServerHelper::new().await;
    let harness = OfflinePlayerHarness::with_sample_rate(
        OfflinePlayerOptions::builder()
            .crossfade_duration(0.0)
            .build(),
        SAMPLE_RATE,
    );
    let spec = SignalSpec {
        format: SignalFormat::Wav,
        length: SignalSpecLength::Frames(FIXTURE_FRAMES),
        channels: CHANNELS,
        sample_rate: SAMPLE_RATE,
        bit_rate: None,
    };
    let url = server.sine(&spec, 440.0).await;
    let public_resource_bus = EventBus::default();
    let mut public_resource_events = public_resource_bus.subscribe();
    let mut config = ResourceConfig::for_src(url.as_str())
        .expect("valid fixture URL")
        .store(StoreOptions::new(temp_dir.path()))
        .byte_pool(harness.player().byte_pool().clone())
        .pcm_pool(harness.player().pcm_pool().clone())
        .build();
    config.set_bus(public_resource_bus);
    config = harness.player().prepare_config(config);
    let mut resource = Resource::new(config).await.expect("open WAV resource");
    let _ = resource.preload().await;
    while public_resource_events.try_recv().is_ok() {}

    harness
        .player()
        .ensure_engine_started()
        .expect("offline engine starts");
    harness
        .player()
        .ensure_slot()
        .expect("offline player slot is allocated");
    harness
        .set_session_tempo(Tempo::new(SESSION_TEMPO).expect("valid session tempo"))
        .expect("session tempo accepted");
    let activation_beat = SessionBeat::new(
        BLOCK_FRAMES.to_f64().expect("block size fits f64") * SESSION_TEMPO
            / (f64::from(SAMPLE_RATE) * 60.0),
    )
    .expect("finite activation beat");
    harness
        .player()
        .insert_with_binding(resource, None, binding(activation_beat), None)
        .await
        .expect("bound fixture inserts after off-path preparation");
    let leaked_seek = from_fn(|| public_resource_events.try_recv().ok()).find(|event| {
        matches!(
            event.event,
            Event::Audio(
                AudioEvent::SeekLifecycle { .. }
                    | AudioEvent::SeekComplete { .. }
                    | AudioEvent::SeekRejected { .. }
            )
        )
    });
    assert!(
        leaked_seek.is_none(),
        "internal preparation must not publish seek events on the application resource bus: {leaked_seek:?}"
    );
    for _ in 0..64 {
        harness.player().set_rate(1.0);
    }
    assert!(matches!(
        harness.player().select_item(0, true),
        Err(PlayError::SlotChannelFull { .. })
    ));
    assert_eq!(harness.player().current_index(), 0);
    assert!(
        harness
            .render(BLOCK_FRAMES)
            .iter()
            .all(|sample| sample.abs() <= f32::EPSILON),
        "a rejected load must not publish audible state"
    );
    harness
        .player()
        .select_item(0, true)
        .expect("the restored bound fixture loads on retry");

    let position_before_seek = harness.player().position_seconds();
    assert!(matches!(
        harness.player().seek_seconds(2.0),
        Err(PlayError::BoundTrackSeekRequiresSessionTransport)
    ));
    assert_eq!(harness.player().position_seconds(), position_before_seek);

    let mut rendered = Vec::with_capacity(BLOCK_FRAMES * 2 * 24);
    let mut events = Vec::new();
    let mut block_peaks = Vec::new();
    for _ in 0..24 {
        let block = harness.render(BLOCK_FRAMES);
        block_peaks.push(
            block
                .iter()
                .fold(0.0_f32, |peak, sample| peak.max(sample.abs())),
        );
        rendered.extend(block);
        events.extend(harness.tick_and_drain());
        sleep(Duration::from_millis(1)).await;
    }

    assert_eq!(rendered.len(), BLOCK_FRAMES * 2 * 24);
    assert!(
        rendered.iter().any(|sample| sample.abs() > 1.0e-4),
        "bound playback must come from the elastic source path; peaks={block_peaks:?}; events={events:?}"
    );
    let frequency = positive_crossing_frequency(&rendered)
        .expect("bound elastic output contains enough complete tone periods");
    assert!(
        (frequency - 440.0).abs() <= 12.0,
        "pitch-preserving elastic output must retain the 440 Hz source tone, got {frequency:.3} Hz"
    );
    let artifact = temp_dir.path().join("bound-elastic-offline.wav");
    write_rendered_wav(&artifact, &rendered);
    assert!(
        fs::metadata(&artifact)
            .expect("offline artifact metadata")
            .len()
            > 44,
        "offline WAV must contain audio data"
    );
}

#[kithara::test(tokio, flash(false))]
async fn resource_blueprint_opens_independent_preparation_readers(temp_dir: TestTempDir) {
    let server = TestServerHelper::new().await;
    let harness = OfflinePlayerHarness::with_sample_rate(
        OfflinePlayerOptions::builder().build(),
        SAMPLE_RATE,
    );
    let spec = SignalSpec {
        format: SignalFormat::Wav,
        length: SignalSpecLength::Frames(BLOCK_FRAMES * 8),
        channels: CHANNELS,
        sample_rate: SAMPLE_RATE,
        bit_rate: None,
    };
    let url = server.sine(&spec, 220.0).await;
    let config = ResourceConfig::for_src(url.as_str())
        .expect("valid fixture URL")
        .store(StoreOptions::new(temp_dir.path()))
        .byte_pool(harness.player().byte_pool().clone())
        .pcm_pool(harness.player().pcm_pool().clone())
        .build();
    let blueprint = ResourceBlueprint::new(harness.player().prepare_config(config));
    let mut active = blueprint.open().await.expect("open active reader");
    let mut prepared = blueprint.open().await.expect("open preparation reader");
    active.preload().await.expect("preload active reader");
    prepared
        .preload()
        .await
        .expect("preload preparation reader");

    drop(active);
    let mut output = [0.0; BLOCK_FRAMES * CHANNELS as usize];
    let read = prepared.read(&mut output).expect("read preparation reader");
    assert!(matches!(read, ReadOutcome::Frames { .. }));
    assert!(output.iter().any(|sample| sample.abs() > 1.0e-4));
}
