use crate::{config::FfiPlayerConfig, player::AudioPlayer};

#[kithara::test]
fn create_player() {
    let _player = AudioPlayer::new(FfiPlayerConfig::default());
}

#[kithara::test]
fn playing_rate_roundtrip() {
    let player = AudioPlayer::new(FfiPlayerConfig::default());
    assert!((player.playing_rate() - 1.0).abs() < f32::EPSILON);
    player.set_playing_rate(0.5);
    assert!((player.playing_rate() - 0.5).abs() < f32::EPSILON);
}

#[kithara::test]
fn items_initially_empty() {
    let player = AudioPlayer::new(FfiPlayerConfig::default());
    assert!(player.items().is_empty());
}

#[kithara::test]
fn remove_all_items_on_empty_queue() {
    let player = AudioPlayer::new(FfiPlayerConfig::default());
    player.remove_all_items();
    assert!(player.items().is_empty());
}

#[kithara::test]
fn volume_roundtrip() {
    let player = AudioPlayer::new(FfiPlayerConfig::default());
    assert!((player.volume() - 1.0).abs() < f32::EPSILON);
    player.set_volume(0.5);
    assert!((player.volume() - 0.5).abs() < f32::EPSILON);
}

#[kithara::test]
fn muted_roundtrip() {
    let player = AudioPlayer::new(FfiPlayerConfig::default());
    assert!(!player.is_muted());
    player.set_muted(true);
    assert!(player.is_muted());
}

#[kithara::test]
fn eq_band_count_from_config() {
    let player = AudioPlayer::new(FfiPlayerConfig {
        eq_band_count: 3,
        ..FfiPlayerConfig::default()
    });
    assert_eq!(player.eq_band_count(), 3);
}

#[kithara::test]
fn eq_gain_default_zero() {
    let player = AudioPlayer::new(FfiPlayerConfig::default());
    assert!((player.eq_gain(0) - 0.0).abs() < f32::EPSILON);
}

#[kithara::test]
#[case::set_gain((|p: &AudioPlayer| p.set_eq_gain(0, 3.0).is_err()) as fn(&AudioPlayer) -> bool)]
#[case::reset((|p: &AudioPlayer| p.reset_eq().is_err()) as fn(&AudioPlayer) -> bool)]
fn eq_mutation_without_engine_returns_error(#[case] op_errs: fn(&AudioPlayer) -> bool) {
    let player = AudioPlayer::new(FfiPlayerConfig::default());
    assert!(op_errs(&player));
}

#[kithara::test]
fn eq_gain_out_of_range_band() {
    let player = AudioPlayer::new(FfiPlayerConfig {
        eq_band_count: 3,
        ..FfiPlayerConfig::default()
    });
    assert!((player.eq_gain(99) - 0.0).abs() < f32::EPSILON);
}

#[kithara::test]
fn current_time_zero_when_no_item() {
    let player = AudioPlayer::new(FfiPlayerConfig::default());
    assert!((player.current_time() - 0.0).abs() < f64::EPSILON);
}

#[kithara::test]
fn current_item_none_when_queue_empty() {
    let player = AudioPlayer::new(FfiPlayerConfig::default());
    assert!(player.current_item().is_none());
}

#[kithara::test]
fn snapshot_uses_playing_rate_field_name() {
    let player = AudioPlayer::new(FfiPlayerConfig::default());
    let snap = player.snapshot();
    assert!((snap.playing_rate - 1.0).abs() < f32::EPSILON);
}
