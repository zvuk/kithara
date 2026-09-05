#![cfg(not(target_arch = "wasm32"))]

use std::num::NonZeroU32;

use kithara::{
    events::{Event, EventBus, EventReceiver, TransportEvent},
    platform::tokio::sync::broadcast::error::TryRecvError,
    play::{Cmd, Reply, SessionBeat, SessionTransportSnapshot, Tempo},
};
use kithara_integration_tests::{
    kithara,
    ring::{ManualRingConfig, ManualRingSession},
};
use num_traits::ToPrimitive;

use crate::bufpool_ext::pools;

const SAMPLE_RATE: u32 = 48_000;

#[derive(Clone, Copy)]
enum CommitCase {
    Tempo(f64),
    Playing(bool),
}

fn session(block_frames: u32, capacity_blocks: usize) -> ManualRingSession {
    let rate = NonZeroU32::new(SAMPLE_RATE).expect("invariant: test sample rate is non-zero");
    ManualRingSession::start(ManualRingConfig::new(rate, block_frames, capacity_blocks))
        .expect("invariant: manual ring session starts")
}

fn expect_ok(reply: Reply) {
    match reply {
        Reply::Ok => {}
        Reply::Err(error) => panic!("session command failed: {error}"),
        _ => panic!("unexpected session command reply"),
    }
}

fn register_transport_events(session: &ManualRingSession) -> EventReceiver {
    let bus = EventBus::default();
    let events = bus.subscribe();
    match session
        .exec(Cmd::RegisterPlayer {
            grid_id: kithara::warp::BeatGridId::allocate().expect("fixture grid id"),
            bus,
            eq_layout: Vec::new(),
            pools: pools(),
            sample_rate: SAMPLE_RATE,
        })
        .expect("invariant: player registration reaches the session")
    {
        Reply::PlayerRegistered(_) => events,
        Reply::Err(error) => panic!("player registration failed: {error}"),
        _ => panic!("unexpected player registration reply"),
    }
}

fn drain_transport_events(events: &mut EventReceiver) -> Vec<TransportEvent> {
    let mut transport = Vec::new();
    loop {
        match events.try_recv().map(|envelope| envelope.event) {
            Ok(Event::Transport(event)) => transport.push(event),
            Ok(_) => {}
            Err(TryRecvError::Empty | TryRecvError::Closed) => break,
            Err(TryRecvError::Lagged(_)) => continue,
        }
    }
    transport
}

fn set_tempo(session: &ManualRingSession, beats_per_minute: f64) {
    let tempo = Tempo::new(beats_per_minute).expect("invariant: test tempo is valid");
    expect_ok(
        session
            .exec(Cmd::SetSessionTempo { tempo })
            .expect("invariant: tempo command reaches the session"),
    );
}

fn set_playing(session: &ManualRingSession, playing: bool) {
    expect_ok(
        session
            .exec(Cmd::SetSessionPlaying { playing })
            .expect("invariant: play-state command reaches the session"),
    );
}

fn snapshot(session: &ManualRingSession) -> SessionTransportSnapshot {
    match session
        .exec(Cmd::QuerySessionTransport)
        .expect("invariant: transport query reaches the session")
    {
        Reply::SessionTransport(snapshot) => snapshot,
        Reply::Err(error) => panic!("transport query failed: {error}"),
        _ => panic!("unexpected transport query reply"),
    }
}

fn commit_initial_transport(
    session: &ManualRingSession,
    events: &mut EventReceiver,
) -> SessionTransportSnapshot {
    set_tempo(session, 120.0);
    session
        .credit(1)
        .expect("invariant: initial transport commit renders");
    let committed = snapshot(session);
    assert_eq!(
        drain_transport_events(events),
        vec![
            TransportEvent::TempoCommitted {
                beats_per_minute: 120.0,
                revision: u64::from(committed.revision()),
            },
            TransportEvent::PlayStateCommitted {
                playing: true,
                revision: u64::from(committed.revision()),
            },
        ]
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
    beats_per_second / f64::from(SAMPLE_RATE)
}

#[kithara::test]
fn transport_commit_is_published_to_every_registered_player_bus() {
    let session = session(512, 2);
    let mut left_events = register_transport_events(&session);
    let mut right_events = register_transport_events(&session);

    set_tempo(&session, 120.0);
    session
        .credit(1)
        .expect("invariant: initial transport commit renders");
    let committed = snapshot(&session);
    let expected = vec![
        TransportEvent::TempoCommitted {
            beats_per_minute: 120.0,
            revision: u64::from(committed.revision()),
        },
        TransportEvent::PlayStateCommitted {
            playing: true,
            revision: u64::from(committed.revision()),
        },
    ];

    assert_eq!(drain_transport_events(&mut left_events), expected);
    assert_eq!(drain_transport_events(&mut right_events), expected);
}

#[kithara::test]
#[case::tempo(CommitCase::Tempo(90.0))]
#[case::pause(CommitCase::Playing(false))]
#[case::redundant_tempo(CommitCase::Tempo(120.0))]
fn transport_commit_announces_only_the_applied_change(#[case] change: CommitCase) {
    let session = session(512, 4);
    let mut events = register_transport_events(&session);
    let initial = commit_initial_transport(&session, &mut events);

    match change {
        CommitCase::Tempo(beats_per_minute) => set_tempo(&session, beats_per_minute),
        CommitCase::Playing(playing) => set_playing(&session, playing),
    }
    session
        .credit(2)
        .expect("invariant: transport change reaches its render boundary");
    let committed = snapshot(&session);
    let published = drain_transport_events(&mut events);

    match change {
        CommitCase::Tempo(beats_per_minute)
            if beats_per_minute == initial.tempo().beats_per_minute() =>
        {
            assert_eq!(committed.revision(), initial.revision());
            assert!(published.is_empty());
        }
        CommitCase::Tempo(beats_per_minute) => assert_eq!(
            published,
            vec![TransportEvent::TempoCommitted {
                beats_per_minute,
                revision: u64::from(committed.revision()),
            }]
        ),
        CommitCase::Playing(playing) => assert_eq!(
            published,
            vec![TransportEvent::PlayStateCommitted {
                playing,
                revision: u64::from(committed.revision()),
            }]
        ),
    }
}

#[kithara::test]
fn seek_commit_announces_the_target_beat() {
    let session = session(512, 4);
    let mut events = register_transport_events(&session);
    let _ = commit_initial_transport(&session, &mut events);
    let target = SessionBeat::new(7.25).expect("invariant: seek target is finite");

    expect_ok(
        session
            .exec(Cmd::SeekSession { target })
            .expect("invariant: seek command reaches the session"),
    );
    session
        .credit(2)
        .expect("invariant: seek reaches its render boundary");
    let committed = snapshot(&session);

    assert_eq!(
        drain_transport_events(&mut events),
        vec![TransportEvent::SeekCommitted {
            position_beats: f64::from(target),
            revision: u64::from(committed.revision()),
        }]
    );
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
    let expected = frames
        .to_f64()
        .expect("invariant: rendered frame count fits f64")
        * 2.0
        / f64::from(SAMPLE_RATE);
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

    set_tempo(&session, 60.0);
    session
        .credit(1)
        .expect("invariant: old tempo reaches the scheduled boundary");
    let boundary = snapshot(&session);
    let old_step = f64::from(BLOCK_FRAMES) * 2.0 / f64::from(SAMPLE_RATE);
    assert_eq!(boundary.revision(), initial.revision());
    assert!(
        (f64::from(boundary.position()) - f64::from(initial.position()) - old_step).abs()
            <= sample_tolerance(2.0)
    );

    session
        .credit(1)
        .expect("invariant: new tempo applies at the boundary");
    let changed = snapshot(&session);
    let new_step = f64::from(BLOCK_FRAMES) / f64::from(SAMPLE_RATE);
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
fn session_seek_relocates_to_the_exact_target_beat() {
    const BLOCK_FRAMES: u32 = 512;
    let session = session(BLOCK_FRAMES, 6);
    set_tempo(&session, 120.0);
    session.credit(1).expect("invariant: initial tempo commits");
    let target = SessionBeat::new(7.25).expect("invariant: seek target is finite");
    expect_ok(
        session
            .exec(Cmd::SeekSession { target })
            .expect("invariant: seek command reaches the session"),
    );
    session
        .credit(1)
        .expect("invariant: active tempo reaches the seek boundary");
    session
        .credit(1)
        .expect("invariant: seek applies at the exact boundary");

    let rendered_step = f64::from(BLOCK_FRAMES) * 2.0 / f64::from(SAMPLE_RATE);
    let relocated_boundary = f64::from(snapshot(&session).position()) - rendered_step;
    assert!((relocated_boundary - f64::from(target)).abs() <= sample_tolerance(2.0));
}

#[kithara::test]
fn paused_transport_holds_its_position_across_rendered_blocks() {
    const BLOCK_FRAMES: u32 = 512;
    let session = session(BLOCK_FRAMES, 8);
    set_tempo(&session, 120.0);
    session.credit(1).expect("invariant: initial tempo commits");
    set_playing(&session, false);
    session
        .credit(1)
        .expect("invariant: playing transport reaches the pause boundary");
    session
        .credit(1)
        .expect("invariant: pause applies at the boundary");
    let paused = snapshot(&session);
    assert!(!paused.is_playing());

    session
        .credit(4)
        .expect("invariant: paused blocks continue rendering");
    let later = snapshot(&session);
    assert!(!later.is_playing());
    assert_eq!(later.position(), paused.position());
}

#[kithara::test]
fn changing_tempo_while_paused_does_not_resume_playback() {
    const BLOCK_FRAMES: u32 = 512;
    let session = session(BLOCK_FRAMES, 10);
    set_tempo(&session, 120.0);
    session.credit(1).expect("invariant: initial tempo commits");
    set_playing(&session, false);
    session
        .credit(2)
        .expect("invariant: pause applies at its boundary");
    let paused = snapshot(&session);
    assert!(!paused.is_playing());

    set_tempo(&session, 90.0);
    session
        .credit(2)
        .expect("invariant: retuned tempo applies at its boundary");
    let retuned = snapshot(&session);

    assert!(!retuned.is_playing());
    assert_eq!(retuned.tempo().beats_per_minute(), 90.0);
    assert_eq!(retuned.position(), paused.position());
}

#[kithara::test]
fn resuming_after_a_pause_continues_from_the_held_position() {
    const BLOCK_FRAMES: u32 = 512;
    let session = session(BLOCK_FRAMES, 12);
    set_tempo(&session, 120.0);
    session.credit(1).expect("invariant: initial tempo commits");
    set_playing(&session, false);
    session
        .credit(2)
        .expect("invariant: pause applies at its boundary");
    let paused = snapshot(&session);
    session
        .credit(3)
        .expect("invariant: paused blocks continue rendering");

    set_playing(&session, true);
    session
        .credit(2)
        .expect("invariant: resume applies at its boundary");
    let resumed = snapshot(&session);

    let step = f64::from(BLOCK_FRAMES) * 2.0 / f64::from(SAMPLE_RATE);
    assert!(resumed.is_playing());
    assert!(
        (f64::from(resumed.position()) - f64::from(paused.position()) - step).abs()
            <= sample_tolerance(2.0),
        "resume must continue from the held beat, not skip the paused span"
    );
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
        Tempo::MIN_BEATS_PER_MINUTE - 0.001,
        Tempo::MAX_BEATS_PER_MINUTE + 0.001,
    ] {
        assert!(
            Tempo::new(invalid).is_err(),
            "tempo {invalid} must be rejected"
        );
    }
    for valid in [
        Tempo::MIN_BEATS_PER_MINUTE,
        120.0,
        Tempo::MAX_BEATS_PER_MINUTE,
    ] {
        assert!(Tempo::new(valid).is_ok(), "tempo {valid} must be accepted");
    }
}
