use kithara_render::bridge::TrackClock;

use super::{core::*, *};

fn clock(track: &mut Track, rig: &mut Rig, clock: TrackClock) -> Option<Seq> {
    rig.with_outbox(|out| track.apply(TrackCommand::Clock(clock), out))
        .expect("live deck scope")
        .expect("the deck has room")
}

#[kithara::test]
async fn a_clock_set_before_the_seat_rides_on_the_attach() {
    let mut rig = rig();
    let mut track = track(A);
    assert_eq!(clock(&mut track, &mut rig, TrackClock::Session), None);

    load(&mut track, &mut rig, Position::ZERO);
    let opened = opened_fixture(&mut rig, "track").await;
    let receipt = rig
        .open(Ok(opened))
        .expect("the load reached the dispatcher");
    rig.with_outbox(|out| track.settle(TrackReceipt::Loaded(receipt), out))
        .expect("live deck scope");
    let receipts = rig.block(frame(0), 0.0);

    assert!(track.attached());
    // The RT keeps the PCM of an applied Attach, so the one seat batch echoes
    // only the clock that rode on it.
    assert!(
        matches!(
            receipts.as_slice(),
            [receipt] if matches!(
                receipt.batch.commands.as_slice(),
                [DeckPart::Clock { slot: A, clock: TrackClock::Session }]
            )
        ),
        "{:?}",
        receipts
            .iter()
            .map(|receipt| &receipt.batch.commands)
            .collect::<Vec<_>>()
    );
}

#[kithara::test]
async fn a_clock_change_reaches_the_seated_track_once() {
    let mut rig = rig();
    let (mut track, _) = loaded(&mut rig, A, "track").await;

    assert!(clock(&mut track, &mut rig, TrackClock::Session).is_some());
    assert_eq!(
        clock(&mut track, &mut rig, TrackClock::Session),
        None,
        "an unchanged clock sends nothing"
    );
    let receipts = rig.block(frame(0), 0.0);

    assert!(
        matches!(
            receipts.as_slice(),
            [receipt] if matches!(
                receipt.batch.commands.as_slice(),
                [DeckPart::Clock { slot: A, clock: TrackClock::Session }]
            )
        ),
        "{:?}",
        receipts
            .iter()
            .map(|receipt| &receipt.batch.commands)
            .collect::<Vec<_>>()
    );
}
