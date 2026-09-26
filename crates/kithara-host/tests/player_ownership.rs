//! A host owns the players inserted into it: only the owning host closes a
//! player, and dropping the host closes every player it still owns.

use kithara_host::{Host, HostConfig, HostOwned};
use kithara_play::{PlayError, PlayWorker, PlayWorkerConfig, PlayerConfig, PlayerImpl};
#[cfg(target_os = "android")]
use kithara_test_dylib as _;
use kithara_test_utils::{
    bufpool::{TestPools, pools},
    kithara,
};
use kithara_warp::BeatGridId;

fn insert_player(host: &mut Host<TestPools>) -> HostOwned<PlayerImpl<TestPools>> {
    let instance_id = BeatGridId::allocate().expect("fixture grid id");
    let player = PlayerImpl::new(
        PlayerConfig::builder()
            .grid_id(instance_id)
            .sample_rate(host.requested_sample_rate())
            .worker(PlayWorker::new(PlayWorkerConfig::builder(pools()).build()))
            .build(),
    );
    let owner = host.insert(player).expect("insert fixture player instance");
    assert_eq!(owner.id(), instance_id);
    owner
}

#[kithara::test]
fn foreign_host_cannot_close_owned_player() {
    let mut owner_host = Host::new(HostConfig::builder().build()).expect("create owner host");
    let mut foreign_host = Host::new(HostConfig::builder().build()).expect("create foreign host");
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
        let mut host = Host::new(HostConfig::builder().build()).expect("create fixture host");
        insert_player(&mut host)
    };

    assert!(
        player.is_closed(),
        "dropping the canonical host must invalidate retained controls"
    );
}
