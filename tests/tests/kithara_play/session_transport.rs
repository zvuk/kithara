#![cfg(not(target_arch = "wasm32"))]

use kithara::{
    bufpool::PcmPool,
    events::{Event, EventBus, EventReceiver, TransportEvent},
    platform::{sync::Arc, tokio::sync::broadcast::error::TryRecvError},
    play::{
        Cmd, EngineConfig, EngineImpl, PlayError, Reply, SessionDispatcher, SessionError,
        SessionHandle, SessionTransportSnapshot, Tempo,
    },
};
use kithara_integration_tests::{kithara, offline::OfflineSession};

const SAMPLE_RATE: u32 = 44_100;

fn engine_config(session: &Arc<OfflineSession>) -> EngineConfig {
    let dispatcher = Arc::clone(session) as Arc<dyn SessionDispatcher>;
    EngineConfig::builder()
        .session(dispatcher)
        .sample_rate(SAMPLE_RATE)
        .pcm_pool(PcmPool::default())
        .build()
}

fn start_engine(session: &Arc<OfflineSession>) -> EngineImpl {
    start_engine_with_bus(session, EventBus::default())
}

fn start_engine_with_bus(session: &Arc<OfflineSession>, bus: EventBus) -> EngineImpl {
    let engine = EngineImpl::new(engine_config(session), bus);
    engine.start().expect("offline engine starts");
    engine
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

fn render_frames(session: &OfflineSession, mut frames: usize, block_frames: usize) {
    while frames > 0 {
        let block = frames.min(block_frames);
        let output = session.render(block);
        assert_eq!(output.len(), block * 2, "offline render is stereo");
        frames -= block;
    }
}

fn position(snapshot: SessionTransportSnapshot) -> f64 {
    snapshot.position().get()
}

fn position_after(block_frames: usize) -> f64 {
    let session = Arc::new(OfflineSession::new_manual());
    let engine = start_engine(&session);
    engine
        .set_session_tempo(Tempo::new(120.0).expect("valid tempo"))
        .expect("tempo accepted");
    render_frames(&session, 22_050, block_frames);
    position(engine.session_transport().expect("transport processed"))
}

#[kithara::test]
fn manual_session_transport_is_shared_and_render_driven() {
    let session = Arc::new(OfflineSession::new_manual());
    let left = start_engine(&session);
    let right = start_engine(&session);

    left.set_session_tempo(Tempo::new(120.0).expect("valid tempo"))
        .expect("tempo accepted");
    assert!(matches!(
        left.session_transport(),
        Err(PlayError::Session(SessionError::TransportNotProcessed))
    ));

    render_frames(&session, 22_050, 512);

    let left_snapshot = left.session_transport().expect("left snapshot");
    let right_snapshot = right.session_transport().expect("right snapshot");
    assert_eq!(left_snapshot, right_snapshot);
    assert!(left_snapshot.is_playing());
    assert_eq!(left_snapshot.tempo(), Tempo::new(120.0).unwrap());
    assert_eq!(left_snapshot.revision(), 1);

    let one_sample = 120.0 / (f64::from(SAMPLE_RATE) * 60.0);
    assert!((position(left_snapshot) - 1.0).abs() <= one_sample);
}

#[kithara::test]
fn transport_commit_is_published_to_every_registered_player_bus() {
    let session = Arc::new(OfflineSession::new_manual());
    let left_bus = EventBus::default();
    let right_bus = EventBus::default();
    let mut left_events = left_bus.subscribe();
    let mut right_events = right_bus.subscribe();
    let left = start_engine_with_bus(&session, left_bus);
    let _right = start_engine_with_bus(&session, right_bus);

    left.set_session_tempo(Tempo::new(120.0).expect("valid tempo"))
        .expect("tempo accepted");
    render_frames(&session, 1, 1);
    let snapshot = left.session_transport().expect("transport commit observed");

    let expected = vec![
        TransportEvent::TempoCommitted {
            beats_per_minute: 120.0,
            revision: snapshot.revision(),
        },
        TransportEvent::PlayStateCommitted {
            playing: true,
            revision: snapshot.revision(),
        },
    ];
    assert_eq!(drain_transport_events(&mut left_events), expected);
    assert_eq!(drain_transport_events(&mut right_events), expected);
}

#[kithara::test]
fn tempo_revision_is_not_observed_before_render_commit() {
    let session = Arc::new(OfflineSession::new_manual());
    let engine = start_engine(&session);

    engine
        .set_session_tempo(Tempo::new(120.0).expect("valid initial tempo"))
        .expect("initial tempo accepted");
    render_frames(&session, 1, 1);
    let committed = engine
        .session_transport()
        .expect("initial tempo is committed");

    engine
        .set_session_tempo(Tempo::new(60.0).expect("valid changed tempo"))
        .expect("tempo change accepted");

    assert_eq!(
        engine
            .session_transport()
            .expect("previous commit remains observable"),
        committed
    );
}

#[kithara::test]
fn tempo_change_preserves_beat_and_changes_slope_at_scheduled_boundary() {
    let session = Arc::new(OfflineSession::new_manual());
    let engine = start_engine(&session);

    engine
        .set_session_tempo(Tempo::new(120.0).expect("valid tempo"))
        .expect("initial tempo accepted");
    render_frames(&session, 11_025, 512);
    let before = position(engine.session_transport().expect("initial snapshot"));
    let initial_revision = engine
        .session_transport()
        .expect("initial revision")
        .revision();

    engine
        .set_session_tempo(Tempo::new(60.0).expect("valid tempo"))
        .expect("tempo change accepted");
    render_frames(&session, 512, 512);
    let before_boundary = engine.session_transport().expect("old boundary snapshot");
    assert_eq!(before_boundary.tempo(), Tempo::new(120.0).unwrap());
    assert_eq!(before_boundary.revision(), initial_revision);

    render_frames(&session, 1, 1);
    let after_one = position(engine.session_transport().expect("changed snapshot"));
    let changed = engine.session_transport().expect("changed revision");
    assert_eq!(changed.tempo(), Tempo::new(60.0).unwrap());
    assert_eq!(changed.revision(), initial_revision + 1);
    let old_slope = 120.0 / (f64::from(SAMPLE_RATE) * 60.0);
    let new_slope = 60.0 / (f64::from(SAMPLE_RATE) * 60.0);
    let tolerance = old_slope.max(new_slope);
    let expected_after_one = before + 512.0 * old_slope + new_slope;
    assert!((after_one - expected_after_one).abs() <= tolerance);

    render_frames(&session, 22_049, 127);
    let after = position(engine.session_transport().expect("final snapshot"));
    let expected = before + 512.0 * old_slope + 0.5;
    assert!((after - expected).abs() <= tolerance);
}

fn render_scheduled_tempo_change(
    blocks: &[usize],
) -> (SessionTransportSnapshot, Vec<SessionTransportSnapshot>) {
    let session = Arc::new(OfflineSession::new_manual());
    let engine = start_engine(&session);
    engine
        .set_session_tempo(Tempo::new(120.0).expect("valid initial tempo"))
        .expect("initial tempo accepted");
    render_frames(&session, 1, 1);
    let initial = engine
        .session_transport()
        .expect("initial tempo is committed");

    engine
        .set_session_tempo(Tempo::new(60.0).expect("valid changed tempo"))
        .expect("tempo change accepted");

    let snapshots = blocks
        .iter()
        .map(|&frames| {
            render_frames(&session, frames, frames);
            engine
                .session_transport()
                .expect("scheduled tempo observation")
        })
        .collect();
    (initial, snapshots)
}

#[kithara::test]
fn scheduled_tempo_change_is_exact_and_offline_partition_independent() {
    let (initial, partitioned) = render_scheduled_tempo_change(&[512, 512]);
    let before_boundary = partitioned[0];
    assert_eq!(before_boundary.tempo(), initial.tempo());
    assert_eq!(before_boundary.revision(), initial.revision());

    let after_boundary = partitioned[1];
    assert_eq!(after_boundary.tempo(), Tempo::new(60.0).unwrap());
    assert_eq!(after_boundary.revision(), initial.revision() + 1);

    let (_, one_shot) = render_scheduled_tempo_change(&[1_024]);
    let one_shot = one_shot[0];
    assert_eq!(one_shot.tempo(), after_boundary.tempo());
    assert_eq!(one_shot.revision(), after_boundary.revision());

    let old_slope = 120.0 / (f64::from(SAMPLE_RATE) * 60.0);
    let new_slope = 60.0 / (f64::from(SAMPLE_RATE) * 60.0);
    let expected = 513.0 * old_slope + 512.0 * new_slope;
    let tolerance = old_slope.max(new_slope);
    assert!((position(after_boundary) - expected).abs() <= tolerance);
    assert!((position(one_shot) - expected).abs() <= tolerance);
    assert!((position(one_shot) - position(after_boundary)).abs() <= tolerance);
}

#[kithara::test]
fn setting_the_same_tempo_does_not_create_a_new_revision() {
    let session = Arc::new(OfflineSession::new_manual());
    let engine = start_engine(&session);
    let tempo = Tempo::new(120.0).expect("valid tempo");
    engine.set_session_tempo(tempo).expect("tempo accepted");
    render_frames(&session, 1, 1);
    let revision = engine.session_transport().unwrap().revision();

    engine
        .set_session_tempo(tempo)
        .expect("equal tempo is a no-op");
    render_frames(&session, 1, 1);
    assert_eq!(engine.session_transport().unwrap().revision(), revision);
}

#[kithara::test]
fn transport_position_is_independent_of_render_partitioning() {
    let expected = position_after(512);
    for block_frames in [64, 127] {
        let actual = position_after(block_frames);
        assert_eq!(actual, expected, "block size {block_frames}");
    }
}

#[kithara::test]
fn tempo_rejects_non_finite_and_non_positive_values() {
    for value in [0.0, -1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert!(Tempo::new(value).is_err(), "tempo {value:?} must fail");
    }
}

#[kithara::test]
fn failed_abort_delivery_is_retried_before_the_next_transport_commit() {
    let session = Arc::new(OfflineSession::new_manual());
    let dispatcher = Arc::clone(&session) as Arc<dyn SessionDispatcher>;
    let handle = SessionHandle::new(dispatcher);
    let player_id = handle
        .register_player(EventBus::default(), Vec::new(), PcmPool::default())
        .expect("player registers");
    handle
        .exec_ok(Cmd::StartPlayer {
            master_volume: 1.0,
            player_id,
            sample_rate: SAMPLE_RATE,
        })
        .expect("player starts");

    let mut delivered_updates = 0;
    loop {
        let volume = if delivered_updates % 2 == 0 {
            0.25
        } else {
            0.75
        };
        handle
            .set_player_master_volume(player_id, volume)
            .expect("volume event queues");
        match handle.exec(Cmd::Tick).expect("tick dispatches") {
            Reply::Ok => delivered_updates += 1,
            Reply::Err(SessionError::Graph(reason)) if reason.contains("MsgChannelFull") => break,
            _ => panic!("unexpected reply while saturating the Firewheel message lane"),
        }
    }
    assert!(delivered_updates > 0);

    let first_tempo = Tempo::new(120.0).expect("valid first tempo");
    assert!(matches!(
        handle
            .exec(Cmd::SetSessionTempo { tempo: first_tempo })
            .expect("tempo dispatches"),
        Reply::Err(SessionError::TransportSync(reason)) if reason == "message channel is full"
    ));

    render_frames(&session, 1, 1);
    assert!(matches!(
        handle
            .exec(Cmd::SessionTransport)
            .expect("transport query dispatches"),
        Reply::Err(SessionError::TransportNotProcessed)
    ));
    render_frames(&session, 1, 1);

    let retry_tempo = Tempo::new(90.0).expect("valid retry tempo");
    assert!(matches!(
        handle
            .exec(Cmd::SetSessionTempo { tempo: retry_tempo })
            .expect("retry dispatches"),
        Reply::Ok
    ));
    render_frames(&session, 1, 1);
    let snapshot = match handle
        .exec(Cmd::SessionTransport)
        .expect("committed transport query dispatches")
    {
        Reply::SessionTransport(snapshot) => snapshot,
        _ => panic!("retry transport did not commit"),
    };
    assert_eq!(snapshot.tempo(), retry_tempo);
    assert_eq!(snapshot.revision(), 2);
}
