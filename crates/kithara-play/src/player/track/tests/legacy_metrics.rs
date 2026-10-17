use super::*;

fn release(track: &mut Track, rig: &mut Rig) -> Vec<Answer> {
    command_at_zero(track, rig, TrackCommand::Release);
    let mut receipts = rig.block(frame(0), 0.0);
    settle(track, rig, &mut receipts);
    rig.opens.drain();
    rig.opens
        .next_due((), 1)
        .expect("release command")
        .apply(Dispatched::Released);
    let receipt = rig.dispatcher.receipts().next().expect("release receipt");
    rig.with_outbox(|out| track.settle(TrackReceipt::Loaded(receipt), out))
        .expect("deck scope");
    receipts
}

#[kithara::test]
fn the_slot_begins_seeks_for_the_tracks_it_shipped() {
    let mut rig = rig();
    let (mut track, mut inbox) = loaded(&mut rig, A, "split.mp3");
    command_at_zero(
        &mut track,
        &mut rig,
        TrackCommand::Seek {
            to: Position::from_secs(30),
        },
    );
    let mut commands = lane_commands(&mut inbox);
    assert_eq!(
        commands
            .iter()
            .filter(|command| matches!(command, LaneCommand::Segment { .. }))
            .count(),
        1
    );
    assert_eq!(
        commands
            .iter()
            .filter(|command| matches!(command, LaneCommand::Jump { .. }))
            .count(),
        0
    );
    let mut receipts = rig.block(frame(0), 0.0);
    settle(&mut track, &mut rig, &mut receipts);
    release(&mut track, &mut rig);
    assert!(
        in_pass(&mut rig, frame(0), |out| track.apply(
            TrackCommand::Seek {
                to: Position::from_secs(45)
            },
            out
        ))
        .is_err()
    );
    commands.extend(lane_commands(&mut inbox));
    assert_eq!(
        commands
            .iter()
            .filter(|command| matches!(command, LaneCommand::Segment { .. }))
            .count(),
        1,
        "an unloaded track must not be seeked any more"
    );
}

#[kithara::test]
fn unloading_one_seek_binding_preserves_other_identity() {
    let mut rig = rig();
    let (mut first, mut first_inbox) = loaded(&mut rig, A, "same.mp3");
    let (mut second, mut second_inbox) = loaded(&mut rig, B, "same.mp3");
    release(&mut first, &mut rig);
    command_at_zero(
        &mut second,
        &mut rig,
        TrackCommand::Seek {
            to: Position::from_secs(30),
        },
    );
    assert_eq!(
        lane_commands(&mut first_inbox).len(),
        0,
        "the unloaded item stays detached"
    );
    let mut second_commands = lane_commands(&mut second_inbox);
    assert_eq!(
        second_commands.len(),
        1,
        "the other queue item keeps its seek path despite sharing the URL"
    );
    let mut receipts = rig.block(frame(0), 0.0);
    settle(&mut second, &mut rig, &mut receipts);
    let mut retired = release(&mut second, &mut rig);
    let (mut replacement, mut replacement_inbox) = loaded(&mut rig, B, "same.mp3");
    settle(&mut replacement, &mut rig, &mut retired);
    command_at_zero(
        &mut replacement,
        &mut rig,
        TrackCommand::Seek {
            to: Position::from_secs(45),
        },
    );
    second_commands.extend(lane_commands(&mut second_inbox));
    assert_eq!(
        second_commands.len(),
        1,
        "the retired resource generation stays detached"
    );
    assert_eq!(
        lane_commands(&mut replacement_inbox).len(),
        1,
        "retiring the old generation must keep its replacement bound"
    );
}
