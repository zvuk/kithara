use kithara_decode::TrackMetadata;
use kithara_platform::{CancelToken, sync::Arc};
use kithara_test_utils::cancel_token;

use super::*;
use crate::Transition;

#[kithara::test(tokio)]
async fn admission_fills_unset_metadata_from_the_decoder(cancel_token: CancelToken) {
    let (mut queue, mut rig, dir) = empty_queue();
    let mut append = |title: Option<&str>| {
        let id = TrackId::allocate();
        let config = ResourceConfig::for_src(ResourceSrc::Path(dir.path().join("entry.wav")))
            .store(
                AssetStore::builder(pools())
                    .backend(StorageBackend::Memory)
                    .build(),
            )
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
        let mut loaded = loaded_fixture(&mut rig, &dir).await;
        loaded.opened.metadata.title = Some("Mock".to_owned());
        answer_loaded(&mut queue, &mut rig, loaded);
        player_internal::finish(&mut queue, &mut rig);
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
}
