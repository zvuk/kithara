use crate::{
    FfiEqBandConfig, FfiEqFilterKind, FfiQueueSettings, config::FfiPlayerConfig,
    player::AudioPlayer, types::FfiPlaybackOrder,
};

#[kithara::test]
fn create_player() {
    let _player = AudioPlayer::new(FfiPlayerConfig::for_test()).expect("create player");
}

#[kithara::test]
fn queue_settings_reach_the_native_owner() {
    let player = AudioPlayer::new_with_queue_settings(
        FfiPlayerConfig::for_test(),
        FfiQueueSettings {
            playback_order: Some(FfiPlaybackOrder::Shuffle),
            max_concurrent_loads: Some(4),
            ..FfiQueueSettings::default()
        },
    )
    .expect("create player with queue settings");
    assert_eq!(player.playback_order(), FfiPlaybackOrder::Shuffle);
}

#[kithara::test]
fn invalid_queue_settings_reject_player_construction() {
    let result = AudioPlayer::new_with_queue_settings(
        FfiPlayerConfig::for_test(),
        FfiQueueSettings {
            max_concurrent_loads: Some(0),
            ..FfiQueueSettings::default()
        },
    );
    assert!(matches!(
        result,
        Err(crate::types::FfiError::InvalidArgument { .. })
    ));
}

#[kithara::test]
fn playing_rate_roundtrip() {
    let player = AudioPlayer::new(FfiPlayerConfig::for_test()).expect("create player");
    assert!((player.playing_rate() - 1.0).abs() < f32::EPSILON);
    player.set_playing_rate(0.5);
    assert!((player.playing_rate() - 0.5).abs() < f32::EPSILON);
}

#[kithara::test]
fn initial_playing_rate_reaches_player_config() {
    let player = AudioPlayer::new(FfiPlayerConfig {
        playing_rate: 0.75,
        ..FfiPlayerConfig::for_test()
    })
    .expect("create player");

    assert_eq!(player.playing_rate(), 0.75);
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
    player.set_volume(0.5);
    assert!((player.volume() - 0.5).abs() < f32::EPSILON);
}

#[kithara::test]
fn muted_roundtrip() {
    let player = AudioPlayer::new(FfiPlayerConfig::for_test()).expect("create player");
    assert!(!player.is_muted());
    player.set_muted(true);
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
fn native_eq_count_above_web_limit_remains_usable() {
    let player = AudioPlayer::new(FfiPlayerConfig {
        eq_band_count: 65,
        ..FfiPlayerConfig::for_test()
    })
    .expect("native EQ layout accepted");
    assert_eq!(player.eq_band_count(), 65);
    player.set_eq_gain(64, 3.0).expect("set last band gain");
    assert_eq!(player.eq_gain(64), 3.0);
    player.reset_eq().expect("reset EQ");
    assert_eq!(player.eq_gain(64), 0.0);
}

#[kithara::test]
fn oversized_eq_config_is_rejected_before_player_construction() {
    let result = AudioPlayer::new(FfiPlayerConfig {
        eq_band_count: u32::MAX,
        ..FfiPlayerConfig::for_test()
    });
    assert!(matches!(
        result,
        Err(crate::types::FfiError::InvalidArgument { .. })
    ));
}

#[kithara::test]
fn native_eq_count_above_resource_budget_is_typed_error() {
    let result = AudioPlayer::new(FfiPlayerConfig {
        eq_band_count: 129,
        ..FfiPlayerConfig::for_test()
    });
    assert!(matches!(
        result,
        Err(crate::types::FfiError::InvalidArgument { .. })
    ));
}

#[kithara::test]
fn generated_eq_layout_reaches_the_player_owner() {
    let player = AudioPlayer::new(FfiPlayerConfig::for_test()).expect("create player");
    player
        .set_eq_layout(vec![FfiEqBandConfig {
            kind: FfiEqFilterKind::Peaking,
            gain_db: 3.0,
            frequency: 440.0,
            q_factor: 0.8,
        }])
        .expect("replace EQ layout");
    assert_eq!(player.eq_band_count(), 1);
    assert_eq!(player.eq_gain(0), 3.0);
}

#[kithara::test]
fn oversized_eq_layout_does_not_replace_the_owner_layout() {
    let player = AudioPlayer::new(FfiPlayerConfig::for_test()).expect("create player");
    let band = FfiEqBandConfig {
        kind: FfiEqFilterKind::Peaking,
        gain_db: 3.0,
        frequency: 440.0,
        q_factor: 0.8,
    };
    assert!(matches!(
        player.set_eq_layout(vec![band; 129]),
        Err(crate::types::FfiError::InvalidArgument { .. })
    ));
    assert_eq!(player.eq_band_count(), 10);
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
    assert_eq!(player.eq_gain(0), 3.0);
    player.reset_eq().expect("reset idle EQ");
    assert_eq!(player.eq_gain(0), 0.0);
    assert!(matches!(
        player.set_eq_gain(99, 3.0),
        Err(crate::types::FfiError::InvalidArgument { .. })
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

struct FailureSignal(std::sync::mpsc::Sender<()>);

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
    let (failed_tx, failed_rx) = std::sync::mpsc::channel();
    player.set_observer(std::sync::Arc::new(FailureSignal(failed_tx)));
    player
        .append(test_item("/nonexistent/kithara-ffi/missing.mp3"))
        .expect("queue accepts the item");
    failed_rx.recv().expect("the missing file fails to load");

    player.play();

    assert_eq!(player.item_count(), 1);
}
