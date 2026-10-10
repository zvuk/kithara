use super::*;
use crate::{
    AdvanceReason, CrossfadeSettings, QueueSettingsChange, Transition,
    queue::state::tests::wait_for_queue_event,
};

#[kithara::test(tokio)]
async fn select_unknown_id_errors() {
    let (mut queue, mut rig, _dir) = empty_queue();
    let err = with_outbox(&mut queue, &mut rig, |queue, out| {
        let output = *out.pass().expect("host pass").output;
        queue.apply_command(
            QueueCommand::Select {
                id: TrackId::allocate(),
                transition: Transition::None,
            },
            Some(&output),
            out,
        )
    })
    .expect_err("unknown id should error");
    assert!(matches!(err, QueueError::UnknownTrackId(_)));
}

#[kithara::test(tokio)]
async fn select_pending_track_stashes_pending_select() {
    let (queue, id, _, _rig, _dir) = pending_selection();
    match queue.target {
        Some(pending) => {
            assert_eq!(pending.to, id);
            assert_eq!(pending.settings.duration, 0.0);
        }
        None => panic!("BUG: select stashes pending entry"),
    }
}

#[kithara::test(tokio)]
async fn advance_to_next_on_empty_emits_queue_ended() {
    let (mut queue, mut rig, _dir) = empty_queue();
    let mut rx = queue.subscribe::<QueueEvent>();
    assert!(
        with_outbox(&mut queue, &mut rig, |queue, out| {
            let output = *out.pass().expect("host pass").output;
            queue.next_target(
                Transition::Crossfade,
                AdvanceReason::NaturalEof,
                true,
                true,
                Some(&output),
                out,
            )
        })
        .expect("BUG: open queue advance must be admitted")
        .is_none()
    );
    tick_fixture(&mut queue, &mut rig);
    let saw_ended =
        wait_for_queue_event(&mut rx, |ev| matches!(ev, QueueEvent::QueueEnded), 200).await;
    assert!(saw_ended);
}

#[kithara::test(tokio)]
async fn manual_next_at_exhaustion_does_not_emit_queue_ended() {
    let (mut queue, mut rig, _dir) = empty_queue();
    let mut rx = queue.subscribe::<QueueEvent>();
    assert_eq!(
        with_outbox(&mut queue, &mut rig, |queue, out| {
            Player::apply(queue, QueueCommand::Next(Transition::None), out)
        })
        .expect("manual next"),
        None
    );
    assert!(!wait_for_queue_event(&mut rx, |ev| matches!(ev, QueueEvent::QueueEnded), 50).await);
}

#[kithara::test(tokio)]
async fn advance_to_next_cycles_then_emits_queue_ended() {
    let (mut queue, first, second, mut rig, dir) = player_internal::selected().await;
    with_outbox(&mut queue, &mut rig, |queue, out| {
        Player::apply(
            queue,
            QueueCommand::Select {
                id: second,
                transition: Transition::None,
            },
            out,
        )
    })
    .expect("select last track");
    answer_load(&mut queue, &mut rig, &dir).await;
    player_internal::finish(&mut queue, &mut rig);
    queue.navigation.select(second, &[first, second]);
    let mut rx = queue.subscribe::<QueueEvent>();
    assert!(
        with_outbox(&mut queue, &mut rig, |queue, out| {
            let output = *out.pass().expect("host pass").output;
            queue.next_target(
                Transition::Crossfade,
                AdvanceReason::NaturalEof,
                true,
                true,
                Some(&output),
                out,
            )
        })
        .expect("BUG: open queue advance must be admitted")
        .is_none()
    );
    tick_fixture(&mut queue, &mut rig);
    let saw_ended =
        wait_for_queue_event(&mut rx, |ev| matches!(ev, QueueEvent::QueueEnded), 400).await;
    assert!(saw_ended, "QueueEnded should be broadcast at end-of-queue");
}

#[kithara::test(tokio)]
async fn admitted_pending_successor_becomes_navigation_authority() {
    let (mut queue, first, second, mut rig, _dir) = player_internal::selected().await;
    queue.navigation.select(first, &[first, second]);
    queue.tracks.set_status(first, TrackStatus::Consumed);
    queue.tracks.set_status(second, TrackStatus::Pending);
    with_outbox(&mut queue, &mut rig, |queue, out| {
        let output = *out.pass().expect("host pass").output;
        queue.next_target(
            Transition::Crossfade,
            AdvanceReason::NaturalEof,
            true,
            true,
            Some(&output),
            out,
        )
    })
    .expect("BUG: open queue advance must be admitted");
    assert_eq!(queue.target.map(|pending| pending.to), Some(second));
    assert_eq!(
        queue.navigation.current(),
        Some(second),
        "admitted automatic successor must remain authoritative while loading"
    );
    let pending = queue
        .target
        .expect("successor selection must remain pending");
    assert_eq!(pending.playing, true);
    assert_eq!(pending.reason, AdvanceReason::NaturalEof);
}

#[kithara::test(tokio)]
async fn pending_override_latches_profile_without_mutating_default() {
    let (mut queue, mut rig, dir) = empty_queue();
    let id = TrackId::allocate();
    let configured =
        CrossfadeSettings::new(2.0, kithara_play::CrossfadeCurve::EqualPower, 1.0, 0.5)
            .expect("valid settings");
    let override_settings =
        CrossfadeSettings::new(4.0, kithara_play::CrossfadeCurve::Linear, 0.25, 0.3)
            .expect("valid settings");
    with_outbox(&mut queue, &mut rig, |queue, out| {
        Player::apply(
            queue,
            QueueCommand::Append {
                id,
                source: TrackSource::Uri(
                    dir.path()
                        .join("entry.wav")
                        .to_str()
                        .expect("fixture path")
                        .to_owned(),
                ),
            },
            out,
        )
        .expect("append fixture");
        Player::apply(
            queue,
            QueueCommand::ConfigureQueue(QueueSettingsChange::Crossfade(configured), When::Next),
            out,
        )
        .expect("valid settings");
        Player::apply(
            queue,
            QueueCommand::Select {
                id,
                transition: Transition::CrossfadeWith {
                    settings: override_settings,
                },
            },
            out,
        )
        .expect("pending selection admitted");
        Player::apply(
            queue,
            QueueCommand::ConfigureQueue(
                QueueSettingsChange::Crossfade(CrossfadeSettings::default()),
                When::Next,
            ),
            out,
        )
        .expect("valid settings");
    });
    let pending = queue.target.expect("selection must remain pending");
    assert_eq!(pending.settings, override_settings);
    assert_eq!(
        queue.control().crossfade_settings(),
        CrossfadeSettings::default()
    );
}
