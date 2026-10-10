#![cfg(not(target_arch = "wasm32"))]

use std::num::NonZeroU32;

use kithara_command::When;
use kithara_events::{EventBus, EventReceiver};
use kithara_platform::tokio::sync::broadcast::error::TryRecvError;
use kithara_play::{SessionTransportSnapshot, Tempo};
use kithara_signal::SessionFrame;
use kithara_test_utils::kithara;
use kithara_warp::BeatGridId;

use super::ring::{ManualRingConfig, ManualRingSession};
use crate::{consts, host::HostSettingsChange, session::TransportEvent};

fn session(block_frames: u32, capacity_blocks: usize) -> ManualRingSession {
    let rate = NonZeroU32::new(consts::RING_ADMISSION_SAMPLE_RATE)
        .expect("invariant: test sample rate is non-zero");
    ManualRingSession::start(ManualRingConfig::new(rate, block_frames, capacity_blocks))
        .expect("invariant: manual ring session starts")
}

fn register_transport_events(session: &ManualRingSession) -> EventReceiver<TransportEvent> {
    let bus = EventBus::default();
    let events = bus.subscribe();
    if let Err(error) = session
        .install(BeatGridId::allocate().expect("fixture grid id"), bus)
        .expect("the deck reaches the graph")
    {
        panic!("the graph refused the deck: {error}");
    }
    events
}

fn drain_transport_events(events: &mut EventReceiver<TransportEvent>) -> Vec<TransportEvent> {
    let mut transport = Vec::new();
    loop {
        match events.try_recv().map(|envelope| envelope.event) {
            Ok(event) => transport.push(event),
            Err(TryRecvError::Empty | TryRecvError::Closed) => break,
            Err(TryRecvError::Lagged(_)) => continue,
        }
    }
    transport
}

fn set_tempo_at(session: &ManualRingSession, beats_per_minute: f64, at: When<SessionFrame>) {
    let tempo = Tempo::new(beats_per_minute).expect("invariant: test tempo is valid");
    if let Err(error) = session
        .configure(HostSettingsChange::Tempo(tempo), at)
        .expect("invariant: tempo command reaches the session")
    {
        panic!("tempo command failed: {error}");
    }
}

fn set_tempo(session: &ManualRingSession, beats_per_minute: f64) {
    set_tempo_at(session, beats_per_minute, When::Next);
}

/// Ticks the session first, as its owner loop does between device blocks, so
/// the receipts of the rendered blocks are settled before the query.
fn snapshot(session: &ManualRingSession) -> SessionTransportSnapshot {
    session
        .tick()
        .expect("invariant: tick reaches the session")
        .expect("the session tick succeeds");
    session
        .transport()
        .expect("invariant: transport read reaches the session")
        .expect("the rendered blocks committed the transport")
}

fn commit_initial_transport(
    session: &ManualRingSession,
    events: &mut EventReceiver<TransportEvent>,
) -> SessionTransportSnapshot {
    session
        .credit(1)
        .expect("invariant: the initial transport renders");
    let committed = snapshot(session);
    assert_eq!(
        drain_transport_events(events),
        Vec::new(),
        "the transport starts at its configured tempo without announcing a change"
    );
    committed
}

fn position(session: &ManualRingSession) -> f64 {
    f64::from(snapshot(session).position())
}

fn clock_samples(session: &ManualRingSession) -> u64 {
    session
        .clock_samples()
        .expect("invariant: manual ring clock is readable")
}

fn sample_tolerance(beats_per_second: f64) -> f64 {
    beats_per_second / f64::from(consts::RING_ADMISSION_SAMPLE_RATE)
}

#[kithara::test]
fn transport_commit_is_published_to_every_registered_player_bus() {
    let session = session(512, 2);
    let mut left_events = register_transport_events(&session);
    let mut right_events = register_transport_events(&session);

    set_tempo(&session, 90.0);
    session
        .credit(1)
        .expect("invariant: the tempo change renders");
    let committed = snapshot(&session);
    let expected = vec![TransportEvent::TempoCommitted {
        beats_per_minute: 90.0,
        revision: u64::from(committed.revision()),
    }];

    assert_eq!(drain_transport_events(&mut left_events), expected);
    assert_eq!(drain_transport_events(&mut right_events), expected);
}

#[kithara::test]
#[case::tempo(90.0)]
#[case::redundant_tempo(120.0)]
fn transport_commit_announces_only_the_applied_change(#[case] beats_per_minute: f64) {
    let session = session(512, 4);
    let mut events = register_transport_events(&session);
    let initial = commit_initial_transport(&session, &mut events);

    set_tempo(&session, beats_per_minute);
    session
        .credit(2)
        .expect("invariant: transport change reaches its render boundary");
    let committed = snapshot(&session);
    let published = drain_transport_events(&mut events);

    if beats_per_minute == initial.tempo().beats_per_minute() {
        assert_eq!(committed.revision(), initial.revision());
        assert!(published.is_empty());
    } else {
        assert_eq!(
            published,
            vec![TransportEvent::TempoCommitted {
                beats_per_minute,
                revision: u64::from(committed.revision()),
            }]
        );
    }
}

#[kithara::test]
fn each_commit_publishes_its_events_once() {
    let session = session(512, 8);
    let mut events = register_transport_events(&session);
    let _ = commit_initial_transport(&session, &mut events);

    set_tempo(&session, 90.0);
    session
        .credit(2)
        .expect("invariant: changed tempo reaches its render boundary");
    let committed = snapshot(&session);
    let expected = vec![TransportEvent::TempoCommitted {
        beats_per_minute: 90.0,
        revision: u64::from(committed.revision()),
    }];
    let mut published = drain_transport_events(&mut events);

    for _ in 0..3 {
        session
            .credit(1)
            .expect("invariant: later blocks continue rendering");
        assert_eq!(snapshot(&session).revision(), committed.revision());
        published.extend(drain_transport_events(&mut events));
    }

    assert_eq!(published, expected);
}

#[kithara::test]
fn session_transport_advances_with_rendered_frames() {
    const BLOCK_FRAMES: u32 = 512;
    const BLOCKS: usize = 7;
    let session = session(BLOCK_FRAMES, BLOCKS);
    set_tempo(&session, 120.0);

    session
        .credit(BLOCKS)
        .expect("invariant: credited blocks render");

    let frames = clock_samples(&session);
    let expected =
        f64::from(u32::try_from(frames).expect("invariant: rendered frame count fits u32")) * 2.0
            / f64::from(consts::RING_ADMISSION_SAMPLE_RATE);
    assert!((position(&session) - expected).abs() <= sample_tolerance(2.0));
}

#[kithara::test]
fn transport_position_is_independent_of_render_partitioning() {
    const TOTAL_FRAMES: u32 = 4_096;
    let mut positions = [0.0; 3];
    for (index, block_frames) in [1_024, 512, 128].into_iter().enumerate() {
        let blocks = usize::try_from(TOTAL_FRAMES / block_frames)
            .expect("invariant: test block count fits usize");
        let session = session(block_frames, blocks);
        set_tempo(&session, 120.0);
        session
            .credit(blocks)
            .expect("invariant: partitioned render completes");
        assert_eq!(clock_samples(&session), u64::from(TOTAL_FRAMES));
        positions[index] = position(&session);
    }

    assert_eq!(positions[0], positions[1]);
    assert_eq!(positions[1], positions[2]);
}

#[kithara::test]
fn tempo_change_preserves_beat_and_changes_slope_at_the_scheduled_boundary() {
    const BLOCK_FRAMES: u32 = 512;
    let session = session(BLOCK_FRAMES, 6);
    set_tempo(&session, 120.0);
    session
        .credit(2)
        .expect("invariant: initial tempo commits and advances");
    let initial = snapshot(&session);

    let boundary_frame = clock_samples(&session) + u64::from(BLOCK_FRAMES);
    set_tempo_at(
        &session,
        60.0,
        When::At(SessionFrame::new(
            i64::try_from(boundary_frame).expect("invariant: the boundary frame fits i64"),
        )),
    );
    session
        .credit(1)
        .expect("invariant: old tempo reaches the scheduled boundary");
    let boundary = snapshot(&session);
    let old_step = f64::from(BLOCK_FRAMES) * 2.0 / f64::from(consts::RING_ADMISSION_SAMPLE_RATE);
    assert_eq!(boundary.revision(), initial.revision());
    assert!(
        (f64::from(boundary.position()) - f64::from(initial.position()) - old_step).abs()
            <= sample_tolerance(2.0)
    );

    session
        .credit(1)
        .expect("invariant: new tempo applies at the boundary");
    let changed = snapshot(&session);
    let elapsed = f64::from(BLOCK_FRAMES) / f64::from(consts::RING_ADMISSION_SAMPLE_RATE);
    let new_step = elapsed + 0.005 * (1.0 - (-elapsed / 0.005).exp());
    assert_eq!(
        u64::from(changed.revision()),
        u64::from(initial.revision()) + 1
    );
    assert_eq!(changed.tempo().beats_per_minute(), 60.0);
    assert!(
        (f64::from(changed.position()) - f64::from(boundary.position()) - new_step).abs()
            <= sample_tolerance(1.0)
    );
}

#[kithara::test]
fn tempo_revision_is_not_observed_before_the_render_commit() {
    const BLOCK_FRAMES: u32 = 512;
    let session = session(BLOCK_FRAMES, 2);
    set_tempo(&session, 120.0);
    session.credit(1).expect("invariant: initial tempo commits");
    let before = snapshot(&session);

    set_tempo(&session, 90.0);

    assert_eq!(snapshot(&session), before);
}

#[kithara::test]
fn setting_the_same_tempo_does_not_create_a_new_revision() {
    const BLOCK_FRAMES: u32 = 512;
    let session = session(BLOCK_FRAMES, 4);
    set_tempo(&session, 120.0);
    set_tempo(&session, 120.0);
    session
        .credit(1)
        .expect("invariant: initial tempo commits once");
    let committed = snapshot(&session);
    assert_eq!(u64::from(committed.revision()), 1);

    set_tempo(&session, 120.0);
    // Render past where a redundant revision would have landed: without this
    // the query would still be reading the pre-command snapshot.
    session
        .credit(2)
        .expect("invariant: a redundant tempo commits nothing");
    let later = snapshot(&session);
    assert_eq!(later.revision(), committed.revision());
    assert_eq!(later.tempo(), committed.tempo());
}

#[kithara::test]
fn tempo_rejects_values_outside_the_representable_range() {
    for invalid in [
        0.0,
        -1.0,
        f64::NAN,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::MAX,
        f64::MIN_POSITIVE,
        f64::from(Tempo::MIN) - 0.001,
        f64::from(Tempo::MAX) + 0.001,
    ] {
        assert!(
            Tempo::new(invalid).is_err(),
            "tempo {invalid} must be rejected"
        );
    }
    for valid in [f64::from(Tempo::MIN), 120.0, f64::from(Tempo::MAX)] {
        assert!(Tempo::new(valid).is_ok(), "tempo {valid} must be accepted");
    }
}
