use kithara_events::TrackId;
use kithara_platform::time::Duration;
use kithara_play::{DeckPart, Player, TrackReceipt, TrackStatus as PlayingStatus};
use kithara_render::bridge::Returned;
use kithara_signal::SessionFrame;
use kithara_test_utils::kithara;

use crate::{
    QueueCommand, QueueEvent, TrackSource, Transition,
    mock::{answer_load, empty_queue, finish, with_outbox},
};

#[kithara::test(tokio)]
#[kithara::hang_watchdog]
async fn notification_changed_carries_src() {
    let (mut queue, mut rig, dir) = empty_queue();
    let path = dir.path().join("next.mp3");
    std::fs::copy(dir.path().join("entry.wav"), &path).expect("named offline fixture");
    let expected_source = path.to_str().expect("fixture path").to_owned();
    let control = queue.control();
    let mut events = control.subscribe();
    let item = TrackId::allocate();
    with_outbox(&mut queue, &mut rig, |queue, out| {
        Player::apply(
            queue,
            QueueCommand::Append {
                position: kithara_play::Position::ZERO,
                id: item,
                source: TrackSource::Uri(expected_source.clone()),
            },
            out,
        )
        .expect("append source");
        Player::apply(
            queue,
            QueueCommand::Select {
                id: item,
                transition: Transition::None,
            },
            out,
        )
        .expect("select source");
    });
    answer_load(&mut queue, &mut rig, &dir).await;
    finish(&mut queue, &mut rig);
    let notification = std::iter::from_fn(|| {
        hang_tick!();
        events.try_recv().ok().map(|event| {
            hang_reset!();
            event.event
        })
    })
    .find(|event| matches!(event, QueueEvent::CurrentTrackChanged { .. }))
    .expect("current source changed");
    let QueueEvent::CurrentTrackChanged { id: Some(id) } = notification else {
        panic!("expected a current identity");
    };
    assert_eq!(id, item);
    let Some(TrackSource::Uri(source)) = control.track_source(id) else {
        panic!("changed identity must retain a URI source");
    };
    assert_eq!(source, expected_source);
}

#[kithara::test(tokio)]
#[kithara::hang_watchdog(timeout = Duration::from_secs(1))]
async fn notification_clone() {
    let (mut queue, mut rig, dir) = empty_queue();
    let path = dir.path().join("ended.mp3");
    std::fs::copy(dir.path().join("entry.wav"), &path).expect("named offline fixture");
    let expected_source = path.to_str().expect("fixture path").to_owned();
    let item = TrackId::allocate();
    with_outbox(&mut queue, &mut rig, |queue, out| {
        Player::apply(
            queue,
            QueueCommand::Append {
                position: kithara_play::Position::ZERO,
                id: item,
                source: TrackSource::Uri(expected_source.clone()),
            },
            out,
        )
        .expect("append stopped source");
        Player::apply(
            queue,
            QueueCommand::Select {
                id: item,
                transition: Transition::None,
            },
            out,
        )
        .expect("select stopped source");
    });
    hang_tick!();
    answer_load(&mut queue, &mut rig, &dir).await;
    hang_reset!();
    finish(&mut queue, &mut rig);
    let slot = queue
        .current_track()
        .expect("current track")
        .snapshot()
        .slot
        .expect("current track holds a slot");
    with_outbox(&mut queue, &mut rig, |queue, out| {
        Player::apply(
            queue,
            QueueCommand::Pause {
                at: kithara_command::When::Next,
            },
            out,
        )
        .expect("explicit stop accepted");
    });
    let mut stopped = false;
    for receipt in rig
        .block(SessionFrame::new(128), 0.0)
        .expect("explicit stop applied")
    {
        hang_tick!();
        let seq = receipt.seq();
        let (outcome, mut batch) = receipt.into();
        stopped |= batch.commands.iter().any(|part| {
            matches!(part, DeckPart::Returned(Returned::Stopped { slot: stopped_slot, resume })
                if *stopped_slot == slot && resume.is_some_and(|resume| resume.position == Duration::ZERO))
        });
        with_outbox(&mut queue, &mut rig, |queue, out| {
            Player::settle(
                queue,
                TrackReceipt::Deck {
                    seq,
                    outcome: &outcome,
                    batch: &mut batch,
                },
                out,
            )
        });
        hang_reset!();
    }
    assert!(
        stopped,
        "explicit stop must return Stopped, not natural EOF"
    );
    let snapshot = Player::snapshot(&queue);
    let cloned = snapshot.clone();
    assert_eq!(snapshot.current, Some(item));
    assert_eq!(cloned.current, Some(item));
    assert!(
        matches!(snapshot.track.as_ref().expect("stopped track").status,
        PlayingStatus::Paused { at } if at == Duration::ZERO)
    );
    assert!(
        matches!(cloned.track.as_ref().expect("cloned stopped track").status,
        PlayingStatus::Paused { at } if at == Duration::ZERO)
    );
    let Some(TrackSource::Uri(source)) = queue
        .control()
        .track_source(snapshot.current.expect("stopped identity"))
    else {
        panic!("stopped identity must retain a URI source");
    };
    assert_eq!(source, expected_source);
    let Some(TrackSource::Uri(source)) = queue
        .control()
        .track_source(cloned.current.expect("cloned stopped identity"))
    else {
        panic!("cloned stopped identity must retain a URI source");
    };
    assert_eq!(source, expected_source);
}
