use kithara_signal::{SessionFrame, TransportRevision};
use kithara_test_utils::kithara;
use kithara_warp::{
    BeatGrid, BeatGridId, BeatGridRevision, BeatGridSnapshot, BeatGridState, SessionAnchor,
    SessionBeat,
};

use super::{
    Accept,
    modes::{
        Group, anchor_at_rate, attach_group, grid_beat_at, group_in, nested, parent_stamp,
        parent_update, rate, sync, synced_deck, tempo_at,
    },
    preparation::{
        asset_grid, asset_segments_from, attach_grid, frontier, prepare, prepare_in, replace_grid,
        window,
    },
};
use crate::{
    SyncAdmission, SyncEffect, SyncError, SyncGroup, SyncIntent, SyncMemberKind, SyncMode,
    SyncPreparation, owner::preparation::Pending,
};

/// A deck following `anchor` as revision one of the parent `parent`.
fn host_deck(parent: BeatGridId, anchor: SessionAnchor) -> Group {
    let mut deck = group_in(SyncMode::HostSync, SyncMemberKind::Grid);
    deck.accept_parent(parent_update(parent_stamp(parent, 1), anchor))
        .expect("the first anchor makes the deck grid live");
    deck
}

pub(super) fn prepared(group: &Group, member: BeatGridId) -> SyncPreparation {
    group
        .pending
        .iter()
        .find_map(|pending| match pending {
            Pending::Prepared { preparation, .. }
                if preparation.stamp().member().grid_id() == member =>
            {
                Some(preparation.clone())
            }
            Pending::Prepared { .. } | Pending::Waiting { .. } => None,
        })
        .expect("the member holds a prepared map")
}

fn activation_beat(preparation: &SyncPreparation) -> f64 {
    let SyncEffect::Projection { alignment, .. } = preparation.effect() else {
        panic!("expected a projection, got {preparation:?}");
    };
    f64::from(*alignment.target().value())
}

fn activation(preparation: &SyncPreparation) -> (u64, SessionFrame) {
    let SyncEffect::Projection { plan, .. } = preparation.effect() else {
        panic!("expected a projection, got {preparation:?}");
    };
    (plan.activation().source(), plan.activation().output())
}

#[kithara::test]
fn a_tempo_commit_before_the_activation_moves_it_onto_the_live_beat() {
    let parent = BeatGridId::allocate().expect("grid id");
    let initial = anchor_at_rate(2.0, 48_000);
    let mut group = host_deck(parent, initial);
    let track = BeatGridId::allocate().expect("grid id");
    attach_grid(&mut group, asset_grid(track, 480_000, 24_000));
    let _ = prepare(&mut group, track, frontier(30_000, 30_000), 0);
    let planned = prepared(&group, track);
    let beat = activation_beat(&planned);
    let session_beat = SessionBeat::new(beat).expect("beat");
    assert_eq!(
        activation(&planned).1,
        initial.frame_at(session_beat).expect("frame")
    );

    let faster = SessionAnchor::new(
        SessionFrame::new(36_000),
        initial.beat_at(SessionFrame::new(36_000)).expect("beat"),
        2.5,
        rate(48_000),
    )
    .expect("tempo anchor");
    let transition = group
        .accept_parent(parent_update(parent_stamp(parent, 2), faster))
        .expect("a tempo commit keeps the session axis");

    let moved = prepared(&group, track);
    assert_eq!(transition.issued(), [moved.clone()]);
    assert!(transition.withdrawn().is_empty());
    assert_eq!(moved.stamp().operation(), planned.stamp().operation());
    assert_eq!(activation(&moved).0, activation(&planned).0);
    assert_eq!(activation_beat(&moved), beat);
    assert!(
        moved.activation().0 > planned.activation().0,
        "the moved activation is a new map"
    );
    assert_eq!(
        activation(&moved).1,
        faster.frame_at(session_beat).expect("frame")
    );
    assert_eq!(moved.stamp().group(), group.snapshot().stamp());
}

/// The parent segment moving `initial` onto `beats_per_second` at `commit`,
/// keeping the beat playing there.
fn retempo(initial: SessionAnchor, commit: i64, beats_per_second: f64) -> SessionAnchor {
    SessionAnchor::new(
        SessionFrame::new(commit),
        initial.beat_at(SessionFrame::new(commit)).expect("beat"),
        beats_per_second,
        rate(48_000),
    )
    .expect("tempo anchor")
}

/// A host deck at 120 BPM holding one prepared track, its parent, and the
/// track.
fn prepared_deck() -> (Group, BeatGridId, BeatGridId) {
    let parent = BeatGridId::allocate().expect("grid id");
    let mut group = host_deck(parent, anchor_at_rate(2.0, 48_000));
    let track = BeatGridId::allocate().expect("grid id");
    attach_grid(&mut group, asset_grid(track, 480_000, 24_000));
    let _ = prepare(&mut group, track, frontier(30_000, 30_000), 0);
    (group, parent, track)
}

#[kithara::test]
fn a_host_preparation_requires_the_parent_processed_output_revision() {
    let parent = BeatGridId::allocate().expect("grid id");
    let revision = TransportRevision::first();
    let mut group = group_in(SyncMode::HostSync, SyncMemberKind::Grid);
    group
        .accept_parent(
            parent_update(parent_stamp(parent, 1), anchor_at_rate(2.0, 48_000))
                .with_output_transport(revision),
        )
        .expect("the processed parent segment is live");
    let track = BeatGridId::allocate().expect("grid id");
    attach_grid(&mut group, asset_grid(track, 480_000, 24_000));
    let _ = prepare(&mut group, track, frontier(30_000, 30_000), 0);

    assert_eq!(
        prepared(&group, track).stamp().output_transport(),
        Some(revision)
    );
}

/// Asserts the group beat `beat` at the activation frame is the beat the
/// member enters on, up to one frame of rounding.
fn assert_on_beat(beat: f64, preparation: &SyncPreparation) {
    let entry = activation_beat(preparation);
    assert!(
        (beat - entry).abs() < 1.0e-4,
        "the activation frame plays beat {beat}, not the entry beat {entry}"
    );
}

pub(super) fn pending_members(group: &Group) -> Vec<BeatGridId> {
    group.pending.iter().map(Pending::member).collect()
}

#[kithara::test]
fn every_pending_member_moves_in_one_transaction_onto_its_own_map() {
    let parent = BeatGridId::allocate().expect("grid id");
    let mut group = host_deck(parent, anchor_at_rate(2.0, 48_000));
    let [first, second] = [(); 2].map(|()| BeatGridId::allocate().expect("grid id"));
    attach_grid(&mut group, asset_grid(first, 480_000, 24_000));
    attach_grid(&mut group, asset_grid(second, 480_000, 20_000));
    let _ = prepare(&mut group, first, frontier(30_000, 30_000), 0);
    let _ = prepare(&mut group, second, frontier(10_000, 30_000), 0);
    let before = [prepared(&group, first), prepared(&group, second)];

    group
        .accept_parent(parent_update(
            parent_stamp(parent, 2),
            retempo(anchor_at_rate(2.0, 48_000), 36_000, 2.5),
        ))
        .expect("a tempo commit keeps the session axis");

    let after = [prepared(&group, first), prepared(&group, second)];
    for (before, after) in before.iter().zip(&after) {
        assert_eq!(after.stamp().operation(), before.stamp().operation());
        assert_eq!(after.stamp().group(), group.snapshot().stamp());
        assert!(after.activation().0 > before.activation().0);
    }
    assert_ne!(
        after[0].activation().0,
        after[1].activation().0,
        "each member gets a map revision of its own"
    );
}

#[kithara::test]
fn an_activation_pushed_past_its_window_is_withdrawn() {
    let (mut group, parent, track) = prepared_deck();
    let planned = activation(&prepared(&group, track)).1;
    let _ = prepare_in(
        &mut group,
        track,
        frontier(30_000, 30_000),
        window(0, i64::from(planned) + 1),
    )
    .expect("the window closes one frame after the activation");
    assert_eq!(activation(&prepared(&group, track)).1, planned);

    group
        .accept_parent(parent_update(
            parent_stamp(parent, 2),
            retempo(anchor_at_rate(2.0, 48_000), 36_000, 1.5),
        ))
        .expect("a slower tempo is still a tempo commit");

    assert_eq!(
        pending_members(&group),
        [],
        "the slower beat lands past the window end"
    );
}

#[kithara::test]
fn a_replaced_member_grid_withdraws_its_preparation() {
    let (mut group, _, track) = prepared_deck();
    let revised = BeatGridSnapshot::segments(
        track,
        BeatGridRevision::first().checked_next().expect("revision"),
        BeatGridState::Complete,
        asset_segments_from(480_000, 480_000, 24_000, 12_000, None),
    )
    .expect("the revised grid is valid");

    replace_grid(&mut group, revised);

    assert_eq!(
        pending_members(&group),
        [],
        "the map was projected from the replaced grid"
    );
}

#[kithara::test]
fn a_local_tempo_commit_moves_the_pending_member() {
    let mut group = synced_deck();
    let track = BeatGridId::allocate().expect("grid id");
    attach_grid(&mut group, asset_grid(track, 480_000, 24_000));
    let _ = prepare(&mut group, track, frontier(30_000, 30_000), 0);
    let planned = prepared(&group, track);

    let _ = group
        .transact(tempo_at(group.id(), 150.0, SessionFrame::new(36_000)))
        .expect("a local deck owns its tempo");

    let moved = prepared(&group, track);
    assert_eq!(moved.stamp().operation(), planned.stamp().operation());
    assert_eq!(activation_beat(&moved), activation_beat(&planned));
    assert_eq!(moved.stamp().group(), group.snapshot().stamp());
    assert_on_beat(
        grid_beat_at(&group, i64::from(activation(&moved).1)),
        &moved,
    );
}

#[kithara::test]
fn a_root_tempo_reaches_the_pending_member_two_levels_down() {
    let mut root = group_in(SyncMode::LocalSync, SyncMemberKind::Group);
    let external_parent = BeatGridId::allocate().expect("parent id");
    let revision = TransportRevision::first();
    let mut middle = group_in(SyncMode::HostSync, SyncMemberKind::Group);
    let deck = group_in(SyncMode::HostSync, SyncMemberKind::Grid);
    let (middle_id, deck_id) = (middle.id(), deck.id());
    attach_group(&mut middle, deck);
    attach_group(&mut root, middle);
    let track = BeatGridId::allocate().expect("grid id");
    let _ = root
        .transact(crate::SyncOperation::Topology {
            base: nested(&root, &[middle_id, deck_id], |deck| {
                deck.topology().expect("topology").stamp()
            }),
            operations: Box::new([crate::TopologyOperation::Attach {
                member: crate::SyncMember::Grid {
                    alignment: None,
                    grid: Box::new(super::TestGrid(asset_grid(track, 480_000, 24_000))),
                },
            }]),
        })
        .expect("the root routes the attach to the deck");
    root.accept_parent(
        parent_update(
            parent_stamp(external_parent, 1),
            anchor_at_rate(2.0, 48_000),
        )
        .with_output_transport(revision),
    )
    .expect("the Local root accepts but does not inherit parent timing");
    let _ = root
        .transact(tempo_at(root.id(), 120.0, SessionFrame::new(0)))
        .expect("the root owns its tempo");
    let _ = prepare(&mut root, track, frontier(30_000, 30_000), 0);
    let read = |root: &Group| {
        nested(root, &[middle_id, deck_id], |deck| {
            let preparation = prepared(deck, deck.pending[0].member());
            let beat = grid_beat_at(deck, i64::from(activation(&preparation).1));
            (preparation, beat, deck.snapshot().stamp())
        })
    };
    let (planned, _, _) = read(&root);
    assert_eq!(
        planned.stamp().output_transport(),
        None,
        "a Local root breaks the global output-transport dependency"
    );

    let admission = root
        .transact(tempo_at(root.id(), 150.0, SessionFrame::new(36_000)))
        .expect("the root owns its tempo");

    let (moved, beat, deck_grid) = read(&root);
    let SyncAdmission::StateChanged { transition, .. } = admission else {
        panic!("expected a state change, got {admission:?}");
    };
    assert_eq!(
        transition.issued(),
        [moved.clone()],
        "the root's tempo commit reports the retarget its grandchild deck issued"
    );
    assert_eq!(moved.stamp().member().grid_id(), track);
    assert_eq!(moved.stamp().operation(), planned.stamp().operation());
    assert_eq!(moved.stamp().group(), deck_grid);
    assert_eq!(moved.stamp().output_transport(), None);
    assert_on_beat(beat, &moved);
    assert!(activation(&moved).1 < activation(&planned).1);
}

#[kithara::test]
fn a_host_parent_output_revision_reaches_a_grandchild_preparation() {
    let external_parent = BeatGridId::allocate().expect("parent id");
    let revision = TransportRevision::first();
    let mut root = group_in(SyncMode::HostSync, SyncMemberKind::Group);
    let mut middle = group_in(SyncMode::HostSync, SyncMemberKind::Group);
    let deck = group_in(SyncMode::HostSync, SyncMemberKind::Grid);
    let (middle_id, deck_id) = (middle.id(), deck.id());
    attach_group(&mut middle, deck);
    attach_group(&mut root, middle);
    let track = BeatGridId::allocate().expect("track id");
    let _ = root
        .transact(crate::SyncOperation::Topology {
            base: nested(&root, &[middle_id, deck_id], |deck| {
                deck.topology().expect("topology").stamp()
            }),
            operations: Box::new([crate::TopologyOperation::Attach {
                member: crate::SyncMember::Grid {
                    alignment: None,
                    grid: Box::new(super::TestGrid(asset_grid(track, 480_000, 24_000))),
                },
            }]),
        })
        .expect("the root routes the attach to the deck");
    root.accept_parent(
        parent_update(
            parent_stamp(external_parent, 1),
            anchor_at_rate(2.0, 48_000),
        )
        .with_output_transport(revision),
    )
    .expect("the host parent segment reaches descendants");
    let _ = prepare(&mut root, track, frontier(30_000, 30_000), 0);
    let prior = nested(&root, &[middle_id, deck_id], |deck| {
        prepared(deck, deck.pending[0].member())
    });

    let child_revision = nested(&root, &[middle_id], |middle| {
        middle
            .parent
            .and_then(super::super::descent::Parent::segment)
            .and_then(|segment| segment.output_transport())
    });
    let grandchild_revision = nested(&root, &[middle_id, deck_id], |deck| {
        prepared(deck, deck.pending[0].member())
            .stamp()
            .output_transport()
    });
    assert_eq!(child_revision, Some(revision));
    assert_eq!(grandchild_revision, Some(revision));

    let next_revision = revision.checked_next().expect("transport revision");
    root.accept_parent(
        parent_update(
            parent_stamp(external_parent, 2),
            anchor_at_rate(2.0, 48_000),
        )
        .with_output_transport(next_revision),
    )
    .expect("the next processed parent revision reaches descendants");
    let reissued = nested(&root, &[middle_id, deck_id], |deck| {
        prepared(deck, deck.pending[0].member())
    });
    assert_eq!(reissued.stamp().operation(), prior.stamp().operation());
    assert_eq!(reissued.stamp().output_transport(), Some(next_revision));
}

#[kithara::test]
fn leaving_the_timeline_withdraws_every_preparation() {
    let (mut group, _, track) = prepared_deck();
    let planned = prepared(&group, track);

    let admission = group
        .transact(sync(group.id(), SyncIntent::Free))
        .expect("free");

    assert_eq!(pending_members(&group), []);
    let SyncAdmission::StateChanged { transition, .. } = admission else {
        panic!("expected a state change, got {admission:?}");
    };
    assert!(transition.issued().is_empty());
    assert_eq!(transition.withdrawn(), [planned.stamp()]);
}

#[kithara::test]
fn a_child_refusing_the_segment_keeps_every_preparation() {
    let mut root = group_in(SyncMode::Off, SyncMemberKind::Group);
    let (accepting, _, _) = prepared_deck();
    let mut refusing = group_in(SyncMode::HostSync, SyncMemberKind::Grid);
    refusing
        .accept_parent(parent_update(
            parent_stamp(root.id(), 5),
            anchor_at_rate(2.0, 48_000),
        ))
        .expect("a later root segment reached this child first");
    let accepting_id = accepting.id();
    attach_group(&mut root, accepting);
    attach_group(&mut root, refusing);
    let read = |root: &Group| nested(root, &[accepting_id], |deck| deck.pending.clone());
    let before = read(&root);

    assert!(matches!(
        root.publish_session(parent_update(
            parent_stamp(root.id(), 2),
            anchor_at_rate(2.5, 48_000),
        )),
        Err(SyncError::StaleGridRevision { .. })
    ));
    assert_eq!(read(&root), before);
}
