//! A host owns the players inserted into it: only the owning host closes a
//! player, and dropping the host closes every player it still owns.

use kithara_host::{Host, HostConfig, HostOwned};
use kithara_play::{PlayError, PlayWorker, PlayWorkerConfig, ResourcePrep};
use kithara_queue::{Queue, QueueConfig};
#[cfg(target_os = "android")]
use kithara_test_dylib as _;
use kithara_test_utils::{
    bufpool::{TestPools, pools},
    kithara,
};

fn insert_player(host: &mut Host<TestPools>) -> HostOwned<Queue<TestPools>> {
    let player = Queue::new(
        QueueConfig::builder()
            .prep(
                ResourcePrep::builder()
                    .worker(PlayWorker::new(PlayWorkerConfig::builder(pools()).build()))
                    .build(),
            )
            .build(),
    );
    let owner = host.insert(player).expect("insert fixture player instance");
    assert_ne!(owner.id(), kithara_warp::BeatGrid::id(host));
    assert!(!host.is_empty());
    owner
}

#[kithara::test]
fn foreign_host_cannot_close_owned_player() {
    let mut owner_host =
        Host::new(HostConfig::offline(pools()).build()).expect("create owner host");
    let mut foreign_host =
        Host::new(HostConfig::offline(pools()).build()).expect("create foreign host");
    let player = insert_player(&mut owner_host);

    let error = foreign_host
        .remove(&player)
        .expect_err("foreign host must reject the player before closing it");
    assert!(matches!(error, PlayError::ForeignSession));
    assert!(
        !player.is_closed(),
        "foreign remove must not close the player"
    );

    owner_host
        .remove(&player)
        .expect("owning host removes its player");
    assert!(player.is_closed());
}

#[kithara::test]
fn dropping_host_invalidates_retained_player_control() {
    let player = {
        let mut host =
            Host::new(HostConfig::offline(pools()).build()).expect("create fixture host");
        insert_player(&mut host)
    };

    assert!(
        player.is_closed(),
        "dropping the canonical host must invalidate retained controls"
    );
}
