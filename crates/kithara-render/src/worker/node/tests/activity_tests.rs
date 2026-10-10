#![cfg(not(target_arch = "wasm32"))]
use kithara_audio::AudioSource;
use kithara_platform::time::Duration;
use kithara_test_utils::kithara;
use kithara_worker::Task;

use crate::mock::node_fixture::NodeFixture;
#[kithara::test(native, tokio)]
async fn playing_for_state_active_states_are_true() {
    let mut fixture = NodeFixture::new(2).await;
    fixture.receiver.set_playing(true);
    for position in 0..7 {
        fixture
            .node
            .source
            .seek(Duration::from_millis(position * 10))
            .expect("active source seek");
        fixture.node.recycle();
        assert!(fixture.activity.is_playing());
    }
}
#[kithara::test(native, tokio)]
async fn playing_for_state_terminal_states_are_false() {
    let mut fixture = NodeFixture::new(2).await;
    for _ in 0..3 {
        fixture.receiver.set_playing(false);
        fixture.node.recycle();
        assert!(!fixture.activity.is_playing());
    }
    fixture.receiver.set_playing(true);
    fixture.node.recycle();
    fixture.node.on_cancel();
    assert!(!fixture.activity.is_playing());
}
#[kithara::test(native, tokio)]
async fn playing_matrix_covers_every_transition_endpoint() {
    let mut fixture = NodeFixture::new(2).await;
    for (position, expected) in [true, true, true, true, true, true, true, false, false]
        .into_iter()
        .enumerate()
    {
        fixture
            .node
            .source
            .seek(Duration::from_millis(
                u64::try_from(position).expect("matrix index") * 10,
            ))
            .expect("source transition");
        fixture.receiver.set_playing(expected);
        fixture.node.recycle();
        assert_eq!(fixture.activity.is_playing(), expected);
    }
}
#[kithara::test(native, tokio)]
async fn no_spurious_flip_across_100_decoding_transitions() {
    let mut fixture = NodeFixture::new(2).await;
    fixture.receiver.set_playing(true);
    for position in 0..100 {
        fixture
            .node
            .source
            .seek(Duration::from_millis(position))
            .expect("source seek preserves activity");
        fixture.node.recycle();
        assert!(
            fixture.activity.is_playing(),
            "PLAYING must stay true across a long Decoding loop"
        );
    }
}
