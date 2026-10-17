use kithara::platform::sync::mpsc;

use crate::{config::FfiPlayerConfig, player::AudioPlayer, types::FfiError};

#[kithara_test_utils::kithara::hang_watchdog(timeout = kithara_platform::time::Duration::from_secs(2))]
fn wait_for_publication(mut published: impl FnMut() -> bool) {
    let deadline =
        kithara_platform::time::WallInstant::now() + kithara_platform::time::Duration::from_secs(2);
    while !published() {
        hang_tick!();
        if kithara_platform::time::WallInstant::now() >= deadline {
            return;
        }
        hang_park!(|_| kithara_platform::thread::paced_backoff(
            kithara_platform::time::Duration::from_millis(5)
        ));
    }
    hang_reset!();
}

#[kithara::test]
fn create_player() {
    crate::mock::offline();
    let _player = AudioPlayer::new(FfiPlayerConfig::for_test()).expect("create player");
}

#[kithara::test]
fn playing_rate_roundtrip() {
    crate::mock::offline();
    let player = AudioPlayer::new(FfiPlayerConfig::for_test()).expect("create player");
    assert!((player.playing_rate() - 1.0).abs() < f32::EPSILON);
    player
        .set_playing_rate(0.5)
        .expect("a finite rate is accepted");
    assert!((player.playing_rate() - 0.5).abs() < f32::EPSILON);
}

#[kithara::test]
fn a_non_numeric_playing_rate_is_refused() {
    crate::mock::offline();
    let player = AudioPlayer::new(FfiPlayerConfig::for_test()).expect("create player");
    let refused = player.set_playing_rate(f32::NAN);
    assert!(
        matches!(refused, Err(FfiError::InvalidArgument { .. })),
        "a non-numeric rate is an invalid argument, not {refused:?}"
    );
    assert!(
        (player.playing_rate() - 1.0).abs() < f32::EPSILON,
        "a refused rate leaves the playing rate as it was, not {}",
        player.playing_rate()
    );
}

#[kithara::test]
fn items_initially_empty() {
    crate::mock::offline();
    let player = AudioPlayer::new(FfiPlayerConfig::for_test()).expect("create player");
    assert!(player.items().is_empty());
}

#[kithara::test]
fn remove_all_items_on_empty_queue() {
    crate::mock::offline();
    let player = AudioPlayer::new(FfiPlayerConfig::for_test()).expect("create player");
    player.remove_all_items();
    assert!(player.items().is_empty());
}

#[kithara::test]
fn volume_roundtrip() {
    crate::mock::offline();
    let player = AudioPlayer::new(FfiPlayerConfig::for_test()).expect("create player");
    assert!((player.volume() - 1.0).abs() < f32::EPSILON);
    player.set_volume(0.5).expect("the player takes the volume");
    let cancel = kithara::platform::CancelScope::new(None);
    crate::mock::render(0..128, &cancel.token()).expect("apply idle volume");
    wait_for_publication(|| (player.volume() - 0.5).abs() < f32::EPSILON);
    assert!((player.volume() - 0.5).abs() < f32::EPSILON);
}

#[kithara::test]
fn muted_roundtrip() {
    crate::mock::offline();
    let player = AudioPlayer::new(FfiPlayerConfig::for_test()).expect("create player");
    assert!(!player.is_muted());
    player.set_muted(true).expect("the player takes the mute");
    let cancel = kithara::platform::CancelScope::new(None);
    crate::mock::render(0..128, &cancel.token()).expect("apply idle mute");
    wait_for_publication(|| player.is_muted());
    assert!(player.is_muted());
}

#[kithara::test]
fn eq_band_count_from_config() {
    crate::mock::offline();
    let player = AudioPlayer::new(FfiPlayerConfig {
        eq_band_count: 3,
        ..FfiPlayerConfig::for_test()
    })
    .expect("create player");
    assert_eq!(player.eq_band_count(), 3);
}

#[kithara::test]
fn eq_gain_default_zero() {
    crate::mock::offline();
    let player = AudioPlayer::new(FfiPlayerConfig::for_test()).expect("create player");
    assert!((player.eq_gain(0) - 0.0).abs() < f32::EPSILON);
}

#[kithara::test]
fn idle_player_eq_can_be_configured_and_reset() {
    crate::mock::offline();
    let player = AudioPlayer::new(FfiPlayerConfig::for_test()).expect("create player");
    player.set_eq_gain(0, 3.0).expect("configure idle EQ");
    let cancel = kithara::platform::CancelScope::new(None);
    crate::mock::render(0..128, &cancel.token()).expect("apply idle EQ");
    wait_for_publication(|| player.eq_gain(0) == 3.0);
    assert_eq!(player.eq_gain(0), 3.0);
    player.reset_eq().expect("reset idle EQ");
    crate::mock::render(128..256, &cancel.token()).expect("apply idle EQ reset");
    wait_for_publication(|| player.eq_gain(0) == 0.0);
    assert_eq!(player.eq_gain(0), 0.0);
    assert!(matches!(
        player.set_eq_gain(99, 3.0),
        Err(FfiError::InvalidArgument { .. })
    ));
}

#[kithara::test]
fn eq_gain_out_of_range_band() {
    crate::mock::offline();
    let player = AudioPlayer::new(FfiPlayerConfig {
        eq_band_count: 3,
        ..FfiPlayerConfig::for_test()
    })
    .expect("create player");
    assert!((player.eq_gain(99) - 0.0).abs() < f32::EPSILON);
}

#[kithara::test]
fn current_time_zero_when_no_item() {
    crate::mock::offline();
    let player = AudioPlayer::new(FfiPlayerConfig::for_test()).expect("create player");
    assert!((player.current_time() - 0.0).abs() < f64::EPSILON);
}

#[kithara::test]
fn current_item_none_when_queue_empty() {
    crate::mock::offline();
    let player = AudioPlayer::new(FfiPlayerConfig::for_test()).expect("create player");
    assert!(player.current_item().is_none());
}

#[kithara::test]
fn snapshot_uses_playing_rate_field_name() {
    crate::mock::offline();
    let player = AudioPlayer::new(FfiPlayerConfig::for_test()).expect("create player");
    let snap = player.snapshot();
    assert!((snap.playing_rate - 1.0).abs() < f32::EPSILON);
}

#[kithara::test]
fn anchorless_insert_goes_to_the_head_and_append_to_the_tail() {
    crate::mock::offline();
    let player = AudioPlayer::new(FfiPlayerConfig::for_test()).expect("create player");
    let inserted = ["https://example.test/a.mp3", "https://example.test/b.mp3"];
    for url in inserted {
        player
            .insert(test_item(url), None)
            .expect("queue accepts the item");
    }
    assert_eq!(player.item_count(), 2);
    let before = player.items();
    let refused = player.replace_item(3, test_item("https://example.test/replacement.mp3"));
    assert!(
        matches!(&refused, Err(FfiError::InvalidArgument { .. })),
        "an absent replacement index is an invalid argument: {refused:?}"
    );
    assert_eq!(player.item_count(), 2);
    assert_eq!(
        player
            .items()
            .iter()
            .map(|item| item.url())
            .collect::<Vec<_>>(),
        before.iter().map(|item| item.url()).collect::<Vec<_>>()
    );
    player
        .append(test_item("https://example.test/c.mp3"))
        .expect("queue accepts the item");

    let queued: Vec<String> = player.items().iter().map(|item| item.url()).collect();
    assert_eq!(
        queued,
        vec![
            "https://example.test/b.mp3".to_owned(),
            "https://example.test/a.mp3".to_owned(),
            "https://example.test/c.mp3".to_owned(),
        ]
    );
}

fn test_item(url: &str) -> std::sync::Arc<crate::item::AudioPlayerItem> {
    crate::item::AudioPlayerItem::new(crate::types::FfiItemConfig::for_test(url))
}

struct FailureSignal(mpsc::Sender<()>);

impl crate::observer::PlayerObserver for FailureSignal {
    fn on_event(&self, event: crate::types::FfiPlayerEvent) {
        if let crate::types::FfiPlayerEvent::TrackStatusChanged {
            status: crate::types::FfiTrackStatus::Failed { .. },
            ..
        } = event
        {
            let _ = self.0.send(());
        }
    }
}

/// The host calls `play()` from its own thread, which has no Tokio runtime.
/// Retrying a failed track must still start its load on the queue's own
/// runtime instead of panicking across the FFI boundary.
#[kithara::test]
fn play_retries_a_failed_track_from_a_thread_without_a_runtime() {
    crate::mock::offline();
    let player = AudioPlayer::new(FfiPlayerConfig::for_test()).expect("create player");
    let (failed_tx, failed_rx) = mpsc::channel();
    player.set_observer(std::sync::Arc::new(FailureSignal(failed_tx)));
    // The queue takes only an absolute path, and what is absolute differs
    // between platforms: a drive-less `/missing.mp3` is not one on Windows.
    let missing = std::env::temp_dir().join("kithara-ffi-missing.mp3");
    let item = test_item(missing.to_str().expect("a UTF-8 temporary path"));
    player.append(item.clone()).expect("queue accepts the item");
    player
        .select(&item, crate::types::FfiTransition::None)
        .expect("the paused queue loads the selected item");
    wait_for_failure(&failed_rx);

    player.play();

    assert_eq!(player.item_count(), 1);
}

#[kithara::allow_block]
#[kithara::flash(io)]
#[kithara_test_utils::kithara::hang_watchdog(timeout = kithara_platform::time::Duration::from_secs(2))]
fn wait_for_failure(receiver: &mpsc::Receiver<()>) {
    let scope = kithara_platform::CancelScope::new(None);
    let mut cursor = 0;
    loop {
        hang_tick!();
        crate::mock::render(cursor..cursor + 512, &scope.token()).expect("offline pull");
        cursor += 512;
        let mut received = false;
        hang_park!(
            |remaining: kithara_platform::time::Duration| match receiver.recv_timeout(
                kithara_platform::time::Instant::now()
                    + remaining.min(kithara_platform::time::Duration::from_millis(5))
            ) {
                Ok(()) => received = true,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    panic!("the missing-file observer disconnected");
                }
            }
        );
        if received {
            hang_reset!();
            return;
        }
    }
}
