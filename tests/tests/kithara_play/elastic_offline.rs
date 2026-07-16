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
        PlayError, PlaybackDirection, PlayerId, Resource, ResourceBlueprint, ResourceConfig,
        SessionBeat, SessionError, SessionTransportSnapshot, SlotId, Tempo, TrackBinding,
        TransportPreparationFailure,
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
const MULTI_TRACK_FIXTURE_FRAMES: usize = 132_300;
const SAMPLE_RATE: u32 = 44_100;
const SESSION_TEMPO: f64 = 120.0;
const CHANGED_SESSION_TEMPO: f64 = 100.0;
const REJECTED_SESSION_TEMPO: f64 = 60.0;
const TRACK_TEMPO: f64 = 100.0;
const TRANSACTION_FRAME_BUDGET: usize = BLOCK_FRAMES * 8;
const SOURCE_TEMPOS: [u32; 4] = [100, 105, 120, 140];
const SOURCE_FREQUENCIES: [f64; 4] = [220.0, 330.0, 440.0, 550.0];
const UNSUPPORTED_PLAYER_ID: PlayerId = 2;

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
    assert_eq!(
        frames_per_minute % u64::from(source_tempo),
        0,
        "source tempo must resolve to an exact integer frame grid"
    );
    let frames_per_beat = frames_per_minute / u64::from(source_tempo);
    let markers = (0..=4).map(|beat| beat * frames_per_beat).collect();
    let analysis = TrackAnalysis::with_source_rate(
        Some(BeatGrid::new(
            f64::from(source_tempo),
            markers,
            vec![0],
            Vec::new(),
        )),
        None,
        u64::try_from(MULTI_TRACK_FIXTURE_FRAMES).expect("fixture frame count fits u64"),
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

fn shared_harnesses(session: &Arc<OfflineSession>, count: usize) -> Vec<OfflinePlayerHarness> {
    (0..count)
        .map(|_| {
            OfflinePlayerHarness::in_session(
                OfflinePlayerOptions::builder()
                    .crossfade_duration(0.0)
                    .build(),
                SAMPLE_RATE,
                Arc::clone(session),
            )
        })
        .collect()
}

async fn insert_bound_sine(
    harness: &OfflinePlayerHarness,
    server: &TestServerHelper,
    temp_dir: &TestTempDir,
    source_tempo: u32,
    frequency: f64,
    session_anchor: SessionBeat,
) {
    let spec = SignalSpec {
        format: SignalFormat::Wav,
        length: SignalSpecLength::Frames(MULTI_TRACK_FIXTURE_FRAMES),
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
    harness
        .player()
        .select_item(0, true)
        .expect("select prepared bound fixture");
}

fn assert_shared_snapshot(
    harnesses: &[OfflinePlayerHarness],
    expected: SessionTransportSnapshot,
    phase: &str,
) {
    for (index, harness) in harnesses.iter().enumerate() {
        assert_eq!(
            harness
                .session_transport()
                .expect("shared session transport snapshot"),
            expected,
            "player {index} observed a different transport during {phase}"
        );
    }
}

fn beat(snapshot: SessionTransportSnapshot) -> f64 {
    snapshot.position().get()
}

async fn assert_each_track_audible(harnesses: &[OfflinePlayerHarness], shared_volume: f32) {
    for harness in harnesses {
        harness.player().set_volume(0.0);
    }
    let _ = harnesses[0].render(BLOCK_FRAMES);

    let mut audible = Vec::with_capacity(harnesses.len());
    let mut peaks = Vec::with_capacity(harnesses.len());
    for harness in harnesses {
        harness.player().set_volume(1.0);
        let mut solo = Vec::with_capacity(BLOCK_FRAMES * usize::from(CHANNELS) * 2);
        let mut block_peaks = Vec::with_capacity(4);
        for block_index in 0..4 {
            let block = harnesses[0].render(BLOCK_FRAMES);
            block_peaks.push(
                block
                    .iter()
                    .fold(0.0_f32, |peak, sample| peak.max(sample.abs())),
            );
            if block_index >= 2 {
                solo.extend(block);
            }
            for participant in harnesses {
                participant.tick_and_drain();
            }
            sleep(Duration::from_millis(1)).await;
        }
        audible.push(solo.iter().any(|sample| sample.abs() > 1.0e-4));
        peaks.push(block_peaks);
        harness.player().set_volume(0.0);
    }
    assert!(
        audible.iter().all(|is_audible| *is_audible),
        "every bound player must remain resident and independently audible before the tempo transaction: audible={audible:?}, peaks={peaks:?}"
    );

    for harness in harnesses {
        harness.player().set_volume(shared_volume);
    }
    let restored = harnesses[0].render(BLOCK_FRAMES);
    assert!(
        restored.iter().any(|sample| sample.abs() > 1.0e-4),
        "the shared mix must be audible after restoring per-player volume"
    );
}

struct PreparedTracks {
    harnesses: Vec<OfflinePlayerHarness>,
    rendered: Vec<f32>,
    slots: Vec<SlotId>,
}

struct CommittedTempoBoundary {
    before: SessionTransportSnapshot,
    after: SessionTransportSnapshot,
    old_frames: usize,
}

fn commit_tempo_transaction(
    harnesses: &[OfflinePlayerHarness],
    rendered: &mut Vec<f32>,
    before: SessionTransportSnapshot,
    tempo: Tempo,
) -> CommittedTempoBoundary {
    harnesses[0]
        .set_session_tempo(tempo)
        .expect("tempo transaction accepted");
    assert_shared_snapshot(harnesses, before, "accepted pending transaction");

    let old_slope = before.tempo().beats_per_minute() / (f64::from(SAMPLE_RATE) * 60.0);
    let new_slope = tempo.beats_per_minute() / (f64::from(SAMPLE_RATE) * 60.0);
    let tolerance = old_slope.max(new_slope);
    let mut before_boundary = before;
    let mut old_frames = 0usize;
    let after_boundary = loop {
        rendered.extend(harnesses[0].render(1));
        harnesses[0]
            .tick_session()
            .expect("tempo transaction progresses without rejection");
        let snapshot = harnesses[0]
            .session_transport()
            .expect("transport while advancing tempo transaction");
        if snapshot.revision() != before.revision() {
            break snapshot;
        }

        old_frames += 1;
        let expected =
            beat(before) + old_frames.to_f64().expect("frame count fits f64") * old_slope;
        assert_eq!(snapshot.tempo(), before.tempo());
        assert!((beat(snapshot) - expected).abs() <= tolerance);
        before_boundary = snapshot;
        assert!(
            old_frames < TRANSACTION_FRAME_BUDGET,
            "tempo transaction did not commit within the deterministic render budget"
        );
    };

    assert_eq!(before_boundary.revision(), before.revision());
    assert_eq!(after_boundary.tempo(), tempo);
    assert!(after_boundary.revision() > before.revision());
    assert_shared_snapshot(harnesses, after_boundary, "committed transaction");

    let expected_before_boundary =
        beat(before) + old_frames.to_f64().expect("frame count fits f64") * old_slope;
    let expected_after_boundary = expected_before_boundary + new_slope;
    assert!((beat(before_boundary) - expected_before_boundary).abs() <= tolerance);
    assert!((beat(after_boundary) - expected_after_boundary).abs() <= tolerance);

    CommittedTempoBoundary {
        before: before_boundary,
        after: after_boundary,
        old_frames,
    }
}

async fn prepare_tracks(
    server: &TestServerHelper,
    temp_dir: &TestTempDir,
    source_tempos: &[u32],
) -> PreparedTracks {
    assert!(!source_tempos.is_empty());
    assert!(source_tempos.len() <= SOURCE_FREQUENCIES.len());
    let session = Arc::new(OfflineSession::new_manual());
    let harnesses = shared_harnesses(&session, source_tempos.len());
    let volume = 1.0 / source_tempos.len().to_f32().expect("track count fits f32");

    let mut slots = Vec::with_capacity(harnesses.len());
    for harness in &harnesses {
        harness
            .player()
            .ensure_engine_started()
            .expect("offline engine starts");
        let slot = harness
            .player()
            .ensure_slot()
            .expect("offline player slot is allocated");
        slots.push(slot);
        harness.player().set_volume(volume);
    }

    harnesses[0]
        .set_session_tempo(Tempo::new(SESSION_TEMPO).expect("valid initial session tempo"))
        .expect("initial session tempo accepted");
    let _ = harnesses[0].render(1);
    let initial = harnesses[0]
        .session_transport()
        .expect("initial transport committed");
    assert_eq!(initial.tempo().beats_per_minute(), SESSION_TEMPO);
    assert_shared_snapshot(&harnesses, initial, "initial commit");

    for (index, &source_tempo) in source_tempos.iter().enumerate() {
        insert_bound_sine(
            &harnesses[index],
            server,
            temp_dir,
            source_tempo,
            SOURCE_FREQUENCIES[index],
            initial.position(),
        )
        .await;
    }

    for _ in 0..16 {
        let _ = harnesses[0].render(BLOCK_FRAMES);
        for harness in &harnesses {
            harness.tick_and_drain();
        }
        sleep(Duration::from_millis(1)).await;
    }
    assert_each_track_audible(&harnesses, volume).await;
    harnesses[0]
        .tick_session()
        .expect("offline session graph tick succeeds");

    PreparedTracks {
        harnesses,
        rendered: Vec::new(),
        slots,
    }
}

struct SuccessfulTransaction {
    rendered: Vec<f32>,
    before: SessionTransportSnapshot,
    before_boundary: SessionTransportSnapshot,
    after_boundary: SessionTransportSnapshot,
    after: SessionTransportSnapshot,
}

async fn run_successful_transaction(
    server: &TestServerHelper,
    temp_dir: &TestTempDir,
    source_tempos: &[u32],
) -> SuccessfulTransaction {
    let PreparedTracks {
        harnesses,
        mut rendered,
        slots: _,
    } = prepare_tracks(server, temp_dir, source_tempos).await;
    let before = harnesses[0]
        .session_transport()
        .expect("transport before tempo transaction");
    assert_eq!(before.tempo().beats_per_minute(), SESSION_TEMPO);
    assert_shared_snapshot(&harnesses, before, "transaction preparation");

    let changed_tempo = Tempo::new(CHANGED_SESSION_TEMPO).expect("valid changed session tempo");
    let boundary = commit_tempo_transaction(&harnesses, &mut rendered, before, changed_tempo);
    let before_boundary = boundary.before;
    let after_boundary = boundary.after;
    assert!(
        boundary.old_frames > 0,
        "tempo commit must use a future boundary"
    );
    assert_eq!(after_boundary.revision(), before.revision() + 1);

    let new_slope = CHANGED_SESSION_TEMPO / (f64::from(SAMPLE_RATE) * 60.0);
    for _ in 0..16 {
        rendered.extend(harnesses[0].render(BLOCK_FRAMES));
        for harness in &harnesses {
            harness.tick_and_drain();
        }
    }
    let after = harnesses[0]
        .session_transport()
        .expect("transport after changed-tempo render");
    let expected_after = beat(after_boundary)
        + 16.0 * BLOCK_FRAMES.to_f64().expect("block size fits f64") * new_slope;
    assert!((beat(after) - expected_after).abs() <= new_slope);
    assert_shared_snapshot(&harnesses, after, "changed-tempo playback");
    assert!(
        rendered.iter().any(|sample| sample.abs() > 1.0e-4),
        "shared bound-track mix must remain audible across the tempo transaction"
    );

    SuccessfulTransaction {
        rendered,
        before,
        before_boundary,
        after_boundary,
        after,
    }
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
async fn two_bound_tracks_commit_one_shared_offline_tempo_transaction(temp_dir: TestTempDir) {
    let server = TestServerHelper::new().await;
    let transaction = run_successful_transaction(&server, &temp_dir, &[100, 120]).await;

    assert_eq!(transaction.before.tempo().beats_per_minute(), SESSION_TEMPO);
    assert_eq!(
        transaction.before_boundary.revision(),
        transaction.before.revision()
    );
    assert_eq!(
        transaction.after_boundary.revision(),
        transaction.before.revision() + 1
    );
    assert_eq!(
        transaction.after.tempo().beats_per_minute(),
        CHANGED_SESSION_TEMPO
    );
    assert!(
        transaction
            .rendered
            .iter()
            .any(|sample| sample.abs() > 1.0e-4)
    );
}

#[kithara::test(tokio, flash(false))]
async fn four_bound_tracks_write_one_shared_offline_tempo_transaction(temp_dir: TestTempDir) {
    let server = TestServerHelper::new().await;
    let transaction = run_successful_transaction(&server, &temp_dir, &SOURCE_TEMPOS).await;

    assert_eq!(
        transaction.after_boundary.revision(),
        transaction.before.revision() + 1
    );
    assert_eq!(
        transaction.after.tempo().beats_per_minute(),
        CHANGED_SESSION_TEMPO
    );
    assert!(
        transaction
            .rendered
            .iter()
            .all(|sample| sample.abs() <= 1.0)
    );

    let artifact = temp_dir.path().join("four-track-tempo-transaction.wav");
    write_rendered_wav(&artifact, &transaction.rendered);
    assert!(
        fs::metadata(&artifact)
            .expect("four-track offline artifact metadata")
            .len()
            > 44,
        "four-track offline WAV must contain the common session mix"
    );
}

#[kithara::test(tokio, flash(false))]
async fn one_unsupported_track_rejects_the_shared_tempo_transaction_and_recovers(
    temp_dir: TestTempDir,
) {
    let server = TestServerHelper::new().await;
    let PreparedTracks {
        harnesses,
        rendered: _,
        slots,
    } = prepare_tracks(&server, &temp_dir, &[90, 100]).await;
    let before = harnesses[0]
        .session_transport()
        .expect("transport before rejected tempo transaction");
    assert_shared_snapshot(&harnesses, before, "rejection preparation");

    harnesses[0]
        .set_session_tempo(
            Tempo::new(REJECTED_SESSION_TEMPO).expect("valid rejected session tempo"),
        )
        .expect("rejected tempo transaction begins asynchronously");
    assert_shared_snapshot(&harnesses, before, "pending rejected transaction");

    let old_slope = SESSION_TEMPO / (f64::from(SAMPLE_RATE) * 60.0);
    let mut rejection_audio = Vec::new();
    let mut rendered_frames = 0usize;
    let rejection = loop {
        rejection_audio.extend(harnesses[0].render(1));
        rendered_frames += 1;
        match harnesses[0].tick_session() {
            Ok(()) => {
                let snapshot = harnesses[0]
                    .session_transport()
                    .expect("old transport remains visible during rejected preparation");
                assert_eq!(snapshot.revision(), before.revision());
                assert_eq!(snapshot.tempo(), before.tempo());
                let expected = beat(before)
                    + rendered_frames.to_f64().expect("frame count fits f64") * old_slope;
                assert!((beat(snapshot) - expected).abs() <= old_slope);
            }
            Err(error) => break error,
        }
        assert!(
            rendered_frames < TRANSACTION_FRAME_BUDGET,
            "unsupported participant did not reject within the deterministic render budget"
        );
    };

    let rejected_revision = match rejection {
        PlayError::Session(SessionError::TransportPreparationRejected {
            player_id,
            revision,
            slot,
            reason: TransportPreparationFailure::UnsupportedRate,
        }) if player_id == UNSUPPORTED_PLAYER_ID && slot == slots[1] => revision,
        other => panic!("expected exact unsupported-rate participant rejection, got {other:?}"),
    };
    assert!(rejected_revision > before.revision());

    rejection_audio.extend(harnesses[0].render(BLOCK_FRAMES));
    rendered_frames += BLOCK_FRAMES;
    harnesses[0]
        .tick_session()
        .expect("rejected participant abort completes on the offline render path");
    let after_rejection = harnesses[0]
        .session_transport()
        .expect("accepted transport remains after participant rejection");
    assert_eq!(after_rejection.revision(), before.revision());
    assert_eq!(after_rejection.tempo(), before.tempo());
    let expected_after_rejection =
        beat(before) + rendered_frames.to_f64().expect("frame count fits f64") * old_slope;
    assert!((beat(after_rejection) - expected_after_rejection).abs() <= old_slope);
    assert_shared_snapshot(&harnesses, after_rejection, "rejected transaction abort");
    assert!(
        rejection_audio.iter().any(|sample| sample.abs() > 1.0e-4),
        "the old shared mix must remain audible while the tempo transaction is rejected"
    );

    let mut recovery_audio = Vec::new();
    let recovery = commit_tempo_transaction(
        &harnesses,
        &mut recovery_audio,
        after_rejection,
        Tempo::new(CHANGED_SESSION_TEMPO).expect("valid recovery session tempo"),
    );
    assert!(recovery.after.revision() > rejected_revision);
    recovery_audio.extend(harnesses[0].render(BLOCK_FRAMES));
    harnesses[0]
        .tick_session()
        .expect("recovered transaction remains healthy");
    assert!(
        recovery_audio.iter().any(|sample| sample.abs() > 1.0e-4),
        "the shared mix must remain audible after a valid recovery transaction"
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
