use super::*;

#[kithara::test]
fn idle_phase_has_no_slot_or_handle() {
    let player = track(A);
    let snapshot = player.snapshot();
    assert!(!snapshot.attached);
    assert!(snapshot.abr.is_none());
    assert!(player.loading.is_none());
    assert!(player.attaching.is_none());
    assert!(player.lane.is_none());
    assert_eq!(snapshot.status, TrackStatus::Idle);
}

#[kithara::test]
fn require_active_slot_errors_from_idle() {
    let mut player = track(A);
    let error = player
        .send_lane(LaneCommand::SetSpeed(SpeedCurve::Constant(1.0)), When::Next)
        .expect_err("an idle track has no active lane");
    assert!(matches!(error, PlayError::NotReady));
}

#[kithara::test]
fn send_to_slot_without_a_slot_is_an_error() {
    let mut player = track(A);
    assert!(
        player
            .send_lane(LaneCommand::SetSpeed(SpeedCurve::Constant(1.0)), When::Next)
            .is_err()
    );
}

#[kithara::test]
fn player_default_rate_getter_setter() {
    let mut player = track(A);
    let mut deck = DeckRig::new(DeckMixerConfig::default()).expect("deck rig");
    assert!((player.projected().speed() - 1.0).abs() < f32::EPSILON);
    deck.with_outbox(|out| {
        player.apply(
            TrackCommand::Configure(TrackSettingsChange::Speed(0.75), When::Next),
            out,
        )
    })
    .expect("outbox")
    .expect("idle speed change");
    assert!((player.projected().speed() - 0.75).abs() < f32::EPSILON);
    assert!((player.settings.projected().speed() - 0.75).abs() < f32::EPSILON);
    assert_eq!(player.snapshot().status, TrackStatus::Idle);
    assert!(!player.snapshot().attached);
    assert!(player.lane.is_none());
    assert!(player.playback_commands.is_empty());
}

#[kithara::test]
fn set_rate_without_active_slot_updates_only_the_requested_target() {
    let mut player = track(A);
    let mut deck = DeckRig::new(DeckMixerConfig::default()).expect("deck rig");
    deck.with_outbox(|out| {
        player.apply(
            TrackCommand::Configure(TrackSettingsChange::Speed(2.0), When::Next),
            out,
        )
    })
    .expect("outbox")
    .expect("idle speed change");
    assert_eq!(player.snapshot().status, TrackStatus::Idle);
    assert!(!player.snapshot().attached);
    assert!(player.lane.is_none());
    assert!(player.playback_commands.is_empty());
    assert!((player.settings.projected().speed() - 2.0).abs() < f32::EPSILON);
}
