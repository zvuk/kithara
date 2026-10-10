use kithara::platform::sync::mpsc;

use crate::{config::FfiPlayerConfig, player::AudioPlayer, types::FfiError};

fn wait_for_publication(mut published: impl FnMut() -> bool) {
    let deadline =
        kithara_platform::time::Instant::now() + kithara_platform::time::Duration::from_secs(5);
    while !published() && kithara_platform::time::Instant::now() < deadline {
        kithara_platform::thread::sleep(kithara_platform::time::Duration::from_millis(5));
    }
}

#[kithara::test]
fn create_player() {
    let _player = AudioPlayer::new(FfiPlayerConfig::for_test()).expect("create player");
}

#[kithara::test]
fn playing_rate_roundtrip() {
    let player = AudioPlayer::new(FfiPlayerConfig::for_test()).expect("create player");
    assert!((player.playing_rate() - 1.0).abs() < f32::EPSILON);
    player
        .set_playing_rate(0.5)
        .expect("a finite rate is accepted");
    assert!((player.playing_rate() - 0.5).abs() < f32::EPSILON);
}

#[kithara::test]
fn a_non_numeric_playing_rate_is_refused() {
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
    let player = AudioPlayer::new(FfiPlayerConfig::for_test()).expect("create player");
    assert!(player.items().is_empty());
}

#[kithara::test]
fn remove_all_items_on_empty_queue() {
    let player = AudioPlayer::new(FfiPlayerConfig::for_test()).expect("create player");
    player.remove_all_items();
    assert!(player.items().is_empty());
}

#[kithara::test]
fn volume_roundtrip() {
    let player = AudioPlayer::new(FfiPlayerConfig::for_test()).expect("create player");
    assert!((player.volume() - 1.0).abs() < f32::EPSILON);
    player.set_volume(0.5).expect("the player takes the volume");
    wait_for_publication(|| (player.volume() - 0.5).abs() < f32::EPSILON);
    assert!((player.volume() - 0.5).abs() < f32::EPSILON);
}

#[kithara::test]
fn muted_roundtrip() {
    let player = AudioPlayer::new(FfiPlayerConfig::for_test()).expect("create player");
    assert!(!player.is_muted());
    player.set_muted(true).expect("the player takes the mute");
    wait_for_publication(|| player.is_muted());
    assert!(player.is_muted());
}

#[kithara::test]
fn eq_band_count_from_config() {
    let player = AudioPlayer::new(FfiPlayerConfig {
        eq_band_count: 3,
        ..FfiPlayerConfig::for_test()
    })
    .expect("create player");
    assert_eq!(player.eq_band_count(), 3);
}

#[kithara::test]
fn eq_gain_default_zero() {
    let player = AudioPlayer::new(FfiPlayerConfig::for_test()).expect("create player");
    assert!((player.eq_gain(0) - 0.0).abs() < f32::EPSILON);
}

#[kithara::test]
fn idle_player_eq_can_be_configured_and_reset() {
    let player = AudioPlayer::new(FfiPlayerConfig::for_test()).expect("create player");
    player.set_eq_gain(0, 3.0).expect("configure idle EQ");
    wait_for_publication(|| player.eq_gain(0) == 3.0);
    assert_eq!(player.eq_gain(0), 3.0);
    player.reset_eq().expect("reset idle EQ");
    wait_for_publication(|| player.eq_gain(0) == 0.0);
    assert_eq!(player.eq_gain(0), 0.0);
    assert!(matches!(
        player.set_eq_gain(99, 3.0),
        Err(FfiError::InvalidArgument { .. })
    ));
}

#[kithara::test]
fn eq_gain_out_of_range_band() {
    let player = AudioPlayer::new(FfiPlayerConfig {
        eq_band_count: 3,
        ..FfiPlayerConfig::for_test()
    })
    .expect("create player");
    assert!((player.eq_gain(99) - 0.0).abs() < f32::EPSILON);
}

#[kithara::test]
fn current_time_zero_when_no_item() {
    let player = AudioPlayer::new(FfiPlayerConfig::for_test()).expect("create player");
    assert!((player.current_time() - 0.0).abs() < f64::EPSILON);
}

#[kithara::test]
fn current_item_none_when_queue_empty() {
    let player = AudioPlayer::new(FfiPlayerConfig::for_test()).expect("create player");
    assert!(player.current_item().is_none());
}

#[kithara::test]
fn snapshot_uses_playing_rate_field_name() {
    let player = AudioPlayer::new(FfiPlayerConfig::for_test()).expect("create player");
    let snap = player.snapshot();
    assert!((snap.playing_rate - 1.0).abs() < f32::EPSILON);
}

#[kithara::test]
fn anchorless_insert_goes_to_the_head_and_append_to_the_tail() {
    let player = AudioPlayer::new(FfiPlayerConfig::for_test()).expect("create player");
    let inserted = ["https://example.test/a.mp3", "https://example.test/b.mp3"];
    for url in inserted {
        player
            .insert(test_item(url), None)
            .expect("queue accepts the item");
    }
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
    let player = AudioPlayer::new(FfiPlayerConfig::for_test()).expect("create player");
    let (failed_tx, failed_rx) = mpsc::channel();
    player.set_observer(std::sync::Arc::new(FailureSignal(failed_tx)));
    // The queue takes only an absolute path, and what is absolute differs
    // between platforms: a drive-less `/missing.mp3` is not one on Windows.
    let missing = std::env::temp_dir().join("kithara-ffi-missing.mp3");
    player
        .append(test_item(missing.to_str().expect("a UTF-8 temporary path")))
        .expect("queue accepts the item");
    wait_for_failure(&failed_rx);

    player.play();

    assert_eq!(player.item_count(), 1);
}

#[kithara::allow_block]
fn wait_for_failure(receiver: &mpsc::Receiver<()>) {
    receiver.recv().expect("the missing file fails to load");
}
