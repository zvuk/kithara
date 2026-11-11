use super::*;

/// What a track publishes about its lane and its place on the deck.
#[derive(Clone, Debug, PartialEq)]
struct Published {
    slot: Option<Slot>,
    status: TrackStatus,
    duration: Option<Duration>,
    abr: bool,
    engine_latency: FrameCount,
    ring_depth: FrameCount,
    pending_lane: bool,
    attached: bool,
    declick: FrameCount,
}

fn published(track: &Track) -> Published {
    let snapshot = track.snapshot();
    Published {
        slot: snapshot.slot,
        status: snapshot.status,
        duration: snapshot.duration,
        abr: snapshot.abr.is_some(),
        engine_latency: snapshot.engine_latency,
        ring_depth: snapshot.ring_depth,
        pending_lane: snapshot.pending_lane,
        attached: snapshot.attached,
        declick: snapshot.declick,
    }
}

async fn open(track: &mut Track, rig: &mut Rig) -> Settled {
    let opened = opened_fixture(rig, "track").await;
    let receipt = rig
        .open(Ok(opened))
        .expect("the load reached the dispatcher");
    rig.with_outbox(|out| track.settle(TrackReceipt::Loaded(receipt), out))
        .expect("live deck scope")
}

fn lane_released(track: &mut Track, rig: &mut Rig) {
    rig.opens.drain();
    let due = rig.opens.next_due((), 1).expect("one lane release");
    assert!(matches!(due.commands(), [DispatcherCommand::Release(_)]));
    due.apply(Dispatched::Released);
    let receipt = rig.dispatcher.receipts().next().expect("release answer");
    rig.with_outbox(|out| track.settle(TrackReceipt::Loaded(receipt), out))
        .expect("live deck scope");
}

/// The track after its load went out: the lane geometry is known, nothing else is.
fn load_sent(idle: &Published, track: &Track) -> Published {
    let sent = published(track);
    assert!(sent.ring_depth > FrameCount::new(0));
    assert!(sent.declick > FrameCount::new(0));
    assert_eq!(
        sent,
        Published {
            status: TrackStatus::Loading,
            ring_depth: sent.ring_depth,
            declick: sent.declick,
            ..idle.clone()
        }
    );
    sent
}

#[kithara::test]
async fn a_seated_track_publishes_each_step_from_load_to_release() {
    let mut rig = rig();
    let mut track = track(A);
    let idle = published(&track);
    assert_eq!(
        idle,
        Published {
            slot: Some(A),
            status: TrackStatus::Idle,
            duration: None,
            abr: false,
            engine_latency: FrameCount::new(0),
            ring_depth: FrameCount::new(0),
            pending_lane: false,
            attached: false,
            declick: FrameCount::new(0),
        }
    );

    load(&mut track, &mut rig, Position::ZERO);
    let sent = load_sent(&idle, &track);

    open(&mut track, &mut rig).await;
    let opened = Published {
        duration: Some(Duration::from_millis(10)),
        attached: true,
        ..sent
    };
    assert_eq!(published(&track), opened, "the attach is on its way");

    let mut receipts = rig.block(frame(0), 0.0);
    settle(&mut track, &mut rig, &mut receipts);
    let attached = Published {
        status: TrackStatus::Loaded,
        ..opened
    };
    assert_eq!(published(&track), attached);

    rig.with_outbox(|out| track.apply(TrackCommand::Release, out))
        .expect("live deck scope")
        .expect("the deck has room");
    assert_eq!(
        published(&track),
        attached,
        "the detach waits for its receipt"
    );

    let mut receipts = rig.block(frame(0), 0.0);
    settle(&mut track, &mut rig, &mut receipts);
    let detached = Published {
        status: TrackStatus::Released,
        attached: false,
        ..attached
    };
    assert_eq!(
        published(&track),
        detached,
        "the slot and the track facts stay"
    );

    lane_released(&mut track, &mut rig);
    assert_eq!(published(&track), detached);
}

#[kithara::test]
async fn a_track_loaded_off_the_deck_publishes_its_seat_once_seated() {
    let mut rig = rig();
    let mut track = track(A);
    track.slot = None;
    let unseated = published(&track);
    assert_eq!(unseated.slot, None);

    load(&mut track, &mut rig, Position::ZERO);
    let sent = load_sent(&unseated, &track);

    open(&mut track, &mut rig).await;
    let reserved = Published {
        status: TrackStatus::Loaded,
        duration: Some(Duration::from_millis(10)),
        ..sent
    };
    assert_eq!(published(&track), reserved, "ready, and off the deck");

    command_at_zero(
        &mut track,
        &mut rig,
        TrackCommand::Seat {
            slot: B,
            at: When::Next,
        },
    );
    let seating = Published {
        slot: Some(B),
        status: TrackStatus::Loading,
        attached: true,
        ..reserved
    };
    assert_eq!(published(&track), seating);

    let mut receipts = rig.block(frame(0), 0.0);
    settle(&mut track, &mut rig, &mut receipts);
    assert_eq!(
        published(&track),
        Published {
            status: TrackStatus::Loaded,
            ..seating
        }
    );
}

#[kithara::test]
fn a_refused_open_returns_the_track_to_idle_with_its_lane_geometry() {
    let mut rig = rig();
    let mut track = track(A);
    let idle = published(&track);

    load(&mut track, &mut rig, Position::ZERO);
    let sent = load_sent(&idle, &track);
    let receipt = rig
        .open(Err(LoadRefusal::Cancelled))
        .expect("the load reached the dispatcher");
    rig.with_outbox(|out| track.settle(TrackReceipt::Loaded(receipt), out))
        .expect("live deck scope");

    assert_eq!(
        published(&track),
        Published {
            status: TrackStatus::Idle,
            ..sent
        }
    );
}

#[kithara::test]
async fn a_second_load_is_refused_and_publishes_nothing_new() {
    let mut rig = rig();
    let mut track = track(A);
    load(&mut track, &mut rig, Position::ZERO);
    open(&mut track, &mut rig).await;
    let mut receipts = rig.block(frame(0), 0.0);
    settle(&mut track, &mut rig, &mut receipts);
    let attached = published(&track);

    let refused = rig
        .with_outbox(|out| {
            track.apply(
                TrackCommand::Load {
                    item: item(),
                    position: Position::ZERO,
                },
                out,
            )
        })
        .expect("live deck scope");

    assert!(
        matches!(refused, Err(PlayError::Internal(_))),
        "{refused:?}"
    );
    assert_eq!(published(&track), attached);
}
