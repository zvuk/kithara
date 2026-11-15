use kithara_decode::TrackMetadata;
use kithara_platform::{CancelToken, sync::Arc};
use kithara_test_utils::cancel_token;

use super::*;
use crate::{Transition, mock::fixture_uri};

#[kithara::test(tokio)]
async fn admission_fills_unset_metadata_from_the_decoder(
    #[by_ref] worker_owner: &WorkerOwner,
    cancel_token: CancelToken,
) {
    let (mut queue, mut rig, dir) = empty_queue(&worker_owner.0);
    let mut append = |title: Option<&str>| {
        let id = TrackId::allocate();
        let config = ResourceConfig::for_src(
            ResourceSrc::parse(fixture_uri(dir.path(), "entry.wav")).expect("fixture URI"),
        )
        .downloader(crate::mock::downloader())
        .store(queue.config.store.as_ref().expect("fixture store").clone())
        .metadata(TrackMetadata {
            title: title.map(Into::into),
            album: Some("Catalogue album".into()),
            ..TrackMetadata::default()
        })
        .build();
        with_outbox(&mut queue, &mut rig, |queue, out| {
            Player::apply(
                queue,
                QueueCommand::Append {
                    position: kithara_play::Position::ZERO,
                    id,
                    source: TrackSource::Config(Box::new(config)),
                },
                out,
            )
        })
        .expect("append configured track");
        id
    };
    let titled = append(Some("Catalogue title"));
    let untitled = append(None);
    let track = |queue: &Queue<TestPools>, id| queue.track(id).expect("the track stays queued");
    assert_eq!(
        track(&queue, titled).metadata().title.as_deref(),
        Some("Catalogue title")
    );
    let cover = Arc::new(vec![1, 2, 3]);
    queue
        .tracks
        .place_cover(titled, &cancel_token, Arc::clone(&cover));
    for id in [titled, untitled] {
        with_outbox(&mut queue, &mut rig, |queue, out| {
            Player::apply(
                queue,
                QueueCommand::Select {
                    id,
                    transition: Transition::None,
                },
                out,
            )
        })
        .expect("select admission fixture");
        let mut loaded = loaded_fixture(
            queue.worker().expect("fixture worker"),
            &mut rig,
            &dir,
            crate::mock::selected_load_seq(&queue),
            FixtureSource::Synthetic,
        )
        .await;
        loaded.opened.metadata.title = Some("Mock".to_owned());
        answer_loaded(&mut queue, &mut rig, loaded);
        crate::mock::finish(&mut queue, &mut rig);
        assert!(matches!(
            track(&queue, id).status,
            TrackStatus::Loaded | TrackStatus::Consumed
        ));
    }
    let admitted = track(&queue, titled);
    let metadata = admitted.metadata();
    assert_eq!(metadata.title.as_deref(), Some("Catalogue title"));
    assert_eq!(metadata.album.as_deref(), Some("Catalogue album"));
    assert_eq!(metadata.artwork, Some(cover));
    assert_eq!(
        track(&queue, untitled).metadata().title.as_deref(),
        Some("Mock")
    );
    with_outbox(&mut queue, &mut rig, |queue, out| {
        HostedDeck::close(queue, out)
    })
    .expect("close the queue before deferred loads run");
    drop((queue, rig));
    dir.finish().await;
}

#[kithara::test(tokio)]
async fn a_synthetic_load_opens_the_decoded_fixture_facts(#[by_ref] worker_owner: &WorkerOwner) {
    let mut opened = Vec::new();
    for source in [FixtureSource::Decoded, FixtureSource::Synthetic] {
        let (queue, _, _, mut rig, dir) = pending_selection(&worker_owner.0);
        let seq = crate::mock::selected_load_seq(&queue);
        let loaded = crate::mock::loaded_request(&queue, &mut rig, seq, source).await;
        let pcm = &loaded.opened.pcm;
        assert_eq!(
            **pcm.src(),
            *fixture_uri(dir.path(), "entry.wav"),
            "{source:?} opens the requested source"
        );
        opened.push((
            loaded.lane,
            loaded.engine_latency,
            loaded.opened.abr.is_none(),
            pcm.spec(),
            pcm.next_chunk_info(),
        ));
        drop((loaded, queue, rig));
        dir.finish().await;
    }
    assert_eq!(
        opened[0], opened[1],
        "a synthetic load answers with the decoded fixture's lane, latency and source facts"
    );
}

#[kithara::test(tokio)]
async fn a_synthetic_load_reads_no_source_bytes(#[by_ref] worker_owner: &WorkerOwner) {
    let (mut queue, mut rig, dir) = empty_queue(&worker_owner.0);
    let unseeded = fixture_uri(dir.path(), "unseeded.wav");
    let id = TrackId::allocate();
    with_outbox(&mut queue, &mut rig, |queue, out| {
        Player::apply(
            queue,
            QueueCommand::Append {
                position: kithara_play::Position::ZERO,
                id,
                source: TrackSource::Uri(unseeded.clone()),
            },
            out,
        )
        .expect("append unseeded track");
        Player::apply(
            queue,
            QueueCommand::Select {
                id,
                transition: Transition::None,
            },
            out,
        )
        .expect("select unseeded track");
    });
    let seq = crate::mock::selected_load_seq(&queue);
    let loaded = crate::mock::loaded_request(&queue, &mut rig, seq, FixtureSource::Synthetic).await;
    assert_eq!(**loaded.opened.pcm.src(), *unseeded);
    assert_eq!(
        loaded.opened.pcm.next_chunk_info(),
        Some(crate::mock::fixture_chunk()),
        "the track opens from memory although its source has no bytes"
    );
    drop((loaded, queue, rig));
    dir.finish().await;
}
