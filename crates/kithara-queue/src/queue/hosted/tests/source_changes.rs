use kithara_render::LoadRefusal;
use kithara_test_utils::kithara;

use super::*;
use crate::{
    Transition,
    mock::{fixture_uri, requested_load, selected, selected_load_seq},
    queue::state::tests::apply,
};

#[kithara::test(tokio)]
async fn a_refused_or_stale_source_keeps_the_accepted_resource(
    #[by_ref] worker_owner: &WorkerOwner,
) {
    let (mut queue, first, second, mut rig, dir) =
        selected(&worker_owner.0, FixtureSource::Synthetic).await;
    let before = queue.control().current().unwrap();
    for id in [first, second, TrackId::allocate()] {
        assert!(
            apply(
                &mut queue,
                &mut rig,
                QueueCommand::ReplaceSource {
                    id,
                    source: "not a URL".into()
                },
            )
            .is_err()
        );
        let after = queue.control().current().unwrap();
        assert_eq!(
            (after.id, after.url, after.status),
            (first, before.url.clone(), before.status.clone())
        );
        assert!(queue.control().is_playing());
        assert!(queue.target.is_none());
        assert!(!queue.tracks.is_changing_source(first));
    }
    drop((queue, rig));
    dir.finish().await;
}

#[kithara::test(tokio)]
async fn a_failed_replacement_load_preserves_the_playing_source(
    #[by_ref] worker_owner: &WorkerOwner,
) {
    let (mut queue, first, _, mut rig, dir) =
        selected(&worker_owner.0, FixtureSource::Synthetic).await;
    let before = queue.control().current().unwrap();
    let mut events = queue.control().subscribe::<QueueEvent>();
    apply(
        &mut queue,
        &mut rig,
        QueueCommand::ReplaceSource {
            id: first,
            source: fixture_uri(dir.path(), "unavailable.wav").into(),
        },
    )
    .unwrap();
    let seq = selected_load_seq(&queue);
    let _request = requested_load(&mut rig, seq);
    rig.opens
        .resume(seq, (), ())
        .unwrap()
        .refuse(LoadRefusal::NoRuntime);
    let receipt = rig.dispatcher.receipts().next().unwrap();
    with_outbox(&mut queue, &mut rig, |queue, out| {
        Player::settle(queue, TrackReceipt::Loaded(receipt), out)
    });
    let after = queue.control().current().unwrap();
    assert_eq!(
        (after.id, after.url, after.status),
        (first, before.url, before.status)
    );
    assert!(!after.source_changing);
    assert!(after.source_failed);
    assert!(queue.control().is_playing());
    assert!(queue.target.is_none());
    assert!(
        std::iter::from_fn(|| events.try_recv().ok()).any(|event| matches!(
            event.event, QueueEvent::TrackLoadFailed { id, auto_skipped: false, .. } if id == first
        ))
    );
    drop((queue, rig));
    dir.finish().await;
}

#[kithara::test(tokio)]
#[case::selection(false)]
#[case::seek(true)]
async fn navigation_cancels_an_uncommitted_source_change(
    #[by_ref] worker_owner: &WorkerOwner,
    #[case] seeking: bool,
) {
    let (mut queue, first, second, mut rig, dir) =
        selected(&worker_owner.0, FixtureSource::Synthetic).await;
    let before = queue.control().current().unwrap();
    apply(
        &mut queue,
        &mut rig,
        QueueCommand::ReplaceSource {
            id: first,
            source: fixture_uri(dir.path(), "new-quality.wav").into(),
        },
    )
    .unwrap();
    apply(
        &mut queue,
        &mut rig,
        if seeking {
            QueueCommand::Seek {
                to: kithara_play::Position::ZERO,
            }
        } else {
            QueueCommand::Select {
                id: second,
                transition: Transition::None,
            }
        },
    )
    .unwrap();

    assert_eq!(queue.control().tracks()[0].url, before.url);
    assert!(!queue.tracks.is_changing_source(first));
    if !seeking {
        answer_load(&mut queue, &mut rig, FixtureSource::Synthetic).await;
        crate::mock::finish(&mut queue, &mut rig);
    }
    assert_eq!(
        queue.control().current().unwrap().id,
        if seeking { first } else { second }
    );
    drop((queue, rig));
    dir.finish().await;
}
