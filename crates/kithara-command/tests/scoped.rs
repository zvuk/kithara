use std::num::{NonZeroU16, NonZeroUsize};

use kithara_command::{
    Batch, ChannelConfig, OpenError, Outcome, Port, Protocol, Rejection, ScopeId, ScopedConfig,
    ScopedInbox, ScopedReceipt, ScopedSender, SendError, Seq, Target, When, scoped_channel,
};
use kithara_test_utils::kithara;

#[derive(Debug, PartialEq, Eq)]
enum Test {}

#[derive(Clone, Copy, Debug)]
struct Slot(usize);

impl Target for Slot {
    fn index(self) -> usize {
        self.0
    }
}

impl Protocol for Test {
    type Applied = ();
    type Clock = u64;
    type Command = u32;
    type Refusal = &'static str;
    type Target = Slot;

    fn frames_since(at: u64, start: u64) -> Option<u64> {
        at.checked_sub(start)
    }
}

fn config(capacity: usize) -> ChannelConfig {
    ChannelConfig::builder()
        .capacity(NonZeroUsize::new(capacity).expect("positive"))
        .targets(2)
        .build()
}

fn pair(capacity: usize, scopes: u16) -> (ScopedSender<Test, Test>, ScopedInbox<Test, Test>) {
    scoped_channel(
        ScopedConfig::builder()
            .root(config(capacity))
            .scope(config(capacity))
            .scopes(NonZeroU16::new(scopes).expect("positive"))
            .build(),
    )
}

fn batch(command: u32) -> Batch<Test> {
    Batch {
        basis: Vec::new(),
        commands: vec![command],
    }
}

fn scope_send(
    sender: &mut ScopedSender<Test, Test>,
    id: ScopeId,
    when: When<u64>,
    command: u32,
) -> Seq {
    sender
        .scope(id)
        .expect("open")
        .send(when, batch(command))
        .expect("room")
}

fn walk(level: &mut kithara_command::LevelInbox<'_, Test>, start: u64) -> Vec<(Seq, usize, u32)> {
    let mut seen = Vec::new();
    while let Some(due) = level.next_due(start, 64) {
        seen.push((due.seq(), due.offset(), due.commands()[0]));
        due.apply(());
    }
    seen
}

#[kithara::test]
fn root_receipt_settles_root_and_preserves_scope_replies() {
    let (mut sender, mut inbox) = pair(1, 1);
    let id = sender.open(1).expect("scope");
    let root_seq = sender.send(When::Next, batch(1)).expect("root room");
    let scope_seq = scope_send(&mut sender, id, When::Next, 2);
    sender.publish().expect("live");
    inbox.drain();
    walk(&mut inbox.scope(id).expect("scope"), 0);
    walk(&mut inbox.root(), 0);
    sender.close(id).expect("close scope");
    sender.publish().expect("publish close");
    inbox.retire_closing();

    assert_eq!(sender.available(), 0);
    let receipt = sender.root_receipt().expect("root receipt");
    assert_eq!(receipt.seq(), root_seq);
    assert_eq!(receipt.outcome(), &Outcome::Applied { at: 0, data: () });
    assert_eq!(sender.available(), 1);
    assert!(sender.root_receipt().is_none());
    let Some(ScopedReceipt::Scope(received_id, receipt)) = sender.receipt() else {
        panic!("the scope receipt remains queued");
    };
    assert_eq!(received_id, id);
    assert_eq!(receipt.seq(), scope_seq);
    assert_eq!(receipt.outcome(), &Outcome::Applied { at: 0, data: () });
    assert!(matches!(sender.receipt(), Some(ScopedReceipt::Closed(closed)) if closed == id));
    assert!(sender.receipt().is_none());
}

#[kithara::test]
fn scoped_commit_eager_stales_and_completion_keeps_original_moment() {
    let (mut sender, mut inbox) = pair(3, 1);
    let id = sender.open(1).expect("scope");
    let mut port = sender.scope(id).expect("scope");
    let committed = port
        .send(
            When::At(80),
            Batch {
                basis: vec![(Slot(0), None)],
                commands: vec![1],
            },
        )
        .expect("room");
    let stale = port
        .send(
            When::At(96),
            Batch {
                basis: vec![(Slot(0), None)],
                commands: vec![2],
            },
        )
        .expect("room");
    let later = port
        .send(
            When::At(100),
            Batch {
                basis: vec![(Slot(0), Some(committed))],
                commands: vec![3],
            },
        )
        .expect("room");
    sender.publish().expect("live");
    inbox.drain();
    inbox
        .scope(id)
        .expect("scope")
        .next_due(64, 64)
        .expect("commit")
        .commit();
    assert_eq!(sender.scope(id).expect("scope").available(), 0);
    let Some(ScopedReceipt::Scope(received_id, receipt)) = sender.receipt() else {
        panic!("eager stale receipt");
    };
    assert_eq!(received_id, id);
    assert_eq!(receipt.seq(), stale);
    assert_eq!(receipt.outcome(), &Outcome::Rejected(Rejection::Stale));
    assert!(sender.receipt().is_none());
    assert_eq!(sender.scope(id).expect("scope").available(), 1);
    inbox
        .scope(id)
        .expect("scope")
        .next_due(64, 64)
        .expect("later shift")
        .apply(());
    let Some(ScopedReceipt::Scope(_, receipt)) = sender.receipt() else {
        panic!("later receipt");
    };
    assert_eq!(receipt.seq(), later);
    assert_eq!(receipt.outcome(), &Outcome::Applied { at: 100, data: () });
    let mut level = inbox.scope(id).expect("scope");
    assert!(level.next_due(256, 64).is_none());
    level.committed_mut(committed).expect("held commands")[0] = 99;
    assert!(level.complete(committed, ()));
    assert!(!level.complete(committed, ()));
    let Some(ScopedReceipt::Scope(_, receipt)) = sender.receipt() else {
        panic!("completed receipt");
    };
    assert_eq!(receipt.seq(), committed);
    assert_eq!(receipt.outcome(), &Outcome::Applied { at: 80, data: () });
    assert_eq!(receipt.batch().commands, [99]);
    assert_eq!(sender.scope(id).expect("scope").available(), 3);
}

#[kithara::test]
fn unfinished_scoped_commits_return_whole_on_retire_or_drop() {
    for retire in [false, true] {
        let (mut sender, mut inbox) = pair(1, 1);
        let id = sender.open(1).expect("scope");
        let root = sender.send(When::Next, batch(1)).expect("root room");
        let scope = scope_send(&mut sender, id, When::Next, 2);
        sender.close(id).expect("reserved");
        sender.publish().expect("live");
        inbox.drain();
        inbox.root().next_due(64, 64).expect("root").commit();
        inbox
            .scope(id)
            .expect("scope")
            .next_due(64, 64)
            .expect("due")
            .commit();
        inbox
            .scope(id)
            .expect("scope")
            .committed_mut(scope)
            .expect("held")[0] = 99;
        assert!(sender.receipt().is_none());
        assert_eq!(sender.scope(id).expect("scope").available(), 0);
        if retire {
            inbox.scope(id).expect("closing").retire();
        } else {
            drop(inbox);
            let Some(ScopedReceipt::Root(receipt)) = sender.receipt() else {
                panic!("root leftover");
            };
            assert_eq!(receipt.seq(), root);
            assert_eq!(receipt.outcome(), &Outcome::Rejected(Rejection::Unanswered));
        }
        let Some(ScopedReceipt::Scope(received_id, receipt)) = sender.receipt() else {
            panic!("scope leftover");
        };
        assert_eq!(received_id, id);
        assert_eq!(receipt.seq(), scope);
        assert_eq!(receipt.outcome(), &Outcome::Rejected(Rejection::Unanswered));
        assert_eq!(receipt.batch().commands, [99]);
        if retire {
            assert!(
                matches!(sender.receipt(), Some(ScopedReceipt::Closed(closed)) if closed == id)
            );
        }
        assert!(sender.receipt().is_none());
    }
}

#[kithara::test]
fn publish_is_one_arrival_cut_for_root_and_all_scopes() {
    let (mut sender, mut inbox) = pair(2, 2);
    let first = sender.open(2).expect("slot");
    let second = sender.open(1).expect("slot");
    let root = sender.send(When::Next, batch(1)).expect("root room");
    inbox.drain();
    assert!(inbox.root().next_due(64, 64).is_none());
    let first_seq = scope_send(&mut sender, first, When::Next, 2);
    inbox.drain();
    assert!(
        inbox
            .scope(first)
            .expect("generation")
            .next_due(64, 64)
            .is_none()
    );
    let second_seq = scope_send(&mut sender, second, When::Next, 3);
    inbox.drain();
    assert!(
        inbox
            .scope(second)
            .expect("generation")
            .next_due(64, 64)
            .is_none()
    );
    assert!(sender.receipt().is_none());
    sender.publish().expect("live");
    inbox.drain();
    assert_eq!(
        walk(&mut inbox.scope(second).expect("generation"), 64),
        [(second_seq, 0, 3)]
    );
    assert_eq!(walk(&mut inbox.root(), 64), [(root, 0, 1)]);
    assert_eq!(
        walk(&mut inbox.scope(first).expect("generation"), 64),
        [(first_seq, 0, 2)]
    );
    let Some(ScopedReceipt::Root(receipt)) = sender.receipt() else {
        panic!("root replies first")
    };
    assert_eq!(receipt.seq(), root);
    assert_eq!(receipt.outcome(), &Outcome::Applied { at: 64, data: () });
    let Some(ScopedReceipt::Scope(id, receipt)) = sender.receipt() else {
        panic!("scope reply")
    };
    assert_eq!((id, receipt.seq()), (second, second_seq));
    let Some(ScopedReceipt::Scope(id, receipt)) = sender.receipt() else {
        panic!("scope reply")
    };
    assert_eq!((id, receipt.seq()), (first, first_seq));
    assert!(sender.receipt().is_none(), "one verdict per batch");
}

#[kithara::test]
fn reclaiming_ring_space_never_publishes_a_partial_pass() {
    let (mut sender, mut inbox) = pair(1, 1);
    let id = sender.open(1).expect("slot");
    for command in [1, 3] {
        sender.send(When::Next, batch(command)).expect("root room");
        scope_send(&mut sender, id, When::Next, command + 1);
        inbox.drain();
        assert!(inbox.root().next_due(0, 64).is_none());
        assert!(inbox.scope(id).expect("scope").next_due(0, 64).is_none());
        sender.publish().expect("live");
        inbox.drain();
        assert_eq!(walk(&mut inbox.root(), 0)[0].2, command);
        assert_eq!(
            walk(&mut inbox.scope(id).expect("scope"), 0)[0].2,
            command + 1
        );
        assert!(sender.receipt().is_some());
        assert!(sender.receipt().is_some());
        assert!(sender.receipt().is_none());
    }
}

#[kithara::test]
fn every_level_orders_by_when_then_shared_seq() {
    let (mut sender, mut inbox) = pair(4, 2);
    let first = sender.open(1).expect("slot");
    let second = sender.open(1).expect("slot");
    let root_late = sender.send(When::At(40), batch(1)).expect("room");
    let first_late = scope_send(&mut sender, first, When::At(40), 2);
    let second_late = scope_send(&mut sender, second, When::At(40), 3);
    let root_next = sender.send(When::Next, batch(4)).expect("room");
    let first_next = scope_send(&mut sender, first, When::Next, 5);
    let second_next = scope_send(&mut sender, second, When::Next, 6);
    let root_equal = sender.send(When::At(40), batch(7)).expect("room");
    let first_early = scope_send(&mut sender, first, When::At(10), 8);
    let second_equal = scope_send(&mut sender, second, When::At(40), 9);
    assert_eq!(
        [
            root_late,
            first_late,
            second_late,
            root_next,
            first_next,
            second_next,
            root_equal,
            first_early,
            second_equal
        ]
        .map(Seq::get),
        [1, 2, 3, 4, 5, 6, 7, 8, 9]
    );
    sender.publish().expect("live");
    inbox.drain();
    assert_eq!(
        walk(&mut inbox.root(), 0),
        [(root_next, 0, 4), (root_late, 40, 1), (root_equal, 40, 7)]
    );
    assert_eq!(
        walk(&mut inbox.scope(first).expect("scope"), 0),
        [
            (first_next, 0, 5),
            (first_early, 10, 8),
            (first_late, 40, 2)
        ]
    );
    assert_eq!(
        walk(&mut inbox.scope(second).expect("scope"), 0),
        [
            (second_next, 0, 6),
            (second_late, 40, 3),
            (second_equal, 40, 9)
        ]
    );
    assert_eq!(std::iter::from_fn(|| sender.receipt()).count(), 9);
}

#[kithara::test]
fn credits_are_per_level_and_close_has_reserved_room() {
    let (mut sender, mut inbox) = pair(1, 2);
    let first = sender.open(1).expect("slot");
    let second = sender.open(1).expect("slot");
    scope_send(&mut sender, first, When::Next, 1);
    assert_eq!(sender.scope(first).expect("scope").available(), 0);
    let Err(SendError::Full(returned)) = sender
        .scope(first)
        .expect("scope")
        .send(When::Next, batch(2))
    else {
        panic!("full level")
    };
    assert_eq!(returned.commands, [2]);
    assert_eq!(sender.available(), 1);
    assert_eq!(sender.scope(second).expect("scope").available(), 1);
    sender.send(When::Next, batch(3)).expect("independent root");
    scope_send(&mut sender, second, When::Next, 4);
    sender.close(first).expect("reserved close credit");
    let Err(SendError::Closed(returned)) = sender
        .scope(first)
        .expect("closing is visible")
        .send(When::Next, batch(5))
    else {
        panic!("closing port")
    };
    assert_eq!(returned.commands, [5]);
    assert!(sender.close(first).is_err());
    sender.publish().expect("live");
    inbox.drain();
    walk(&mut inbox.root(), 0);
    assert_eq!(
        sender.available(),
        0,
        "receipt must be read to return credit"
    );
    assert!(matches!(sender.receipt(), Some(ScopedReceipt::Root(_))));
    assert_eq!(sender.available(), 1);
    walk(&mut inbox.scope(second).expect("scope"), 0);
    walk(&mut inbox.scope(first).expect("scope"), 0);
    inbox.scope(first).expect("closing").retire();
    assert_eq!(std::iter::from_fn(|| sender.receipt()).count(), 3);
    assert_eq!(sender.scope(second).expect("scope").available(), 1);
}

#[kithara::test]
fn final_next_applies_before_retire_and_closed_is_the_last_answer() {
    let (mut sender, mut inbox) = pair(4, 1);
    let id = sender.open(1).expect("slot");
    let parked = scope_send(&mut sender, id, When::Deferred, 1);
    sender.publish().expect("live");
    inbox.drain();
    assert_eq!(
        inbox
            .scope(id)
            .expect("scope")
            .next_deferred()
            .expect("arrival")
            .park(),
        Some(parked)
    );
    let arrived = scope_send(&mut sender, id, When::Deferred, 2);
    let future = scope_send(&mut sender, id, When::At(200), 3);
    let final_next = scope_send(&mut sender, id, When::Next, 4);
    sender.close(id).expect("reserved");
    sender.publish().expect("live");
    inbox.drain();
    inbox.drain();
    assert!(inbox.scope(id).expect("not auto-retired").is_closing());
    assert!(!inbox.root().is_closing());
    assert_eq!(
        walk(&mut inbox.scope(id).expect("scope"), 64),
        [(final_next, 0, 4)]
    );
    inbox.scope(id).expect("closing").retire();
    assert!(inbox.scope(id).is_none());
    assert!(matches!(
        sender.open(3),
        Err(OpenError::Targets {
            targets: 3,
            limit: 2
        })
    ));
    assert!(matches!(sender.open(1), Err(OpenError::Exhausted)));
    for (seq, outcome) in [
        (final_next, Outcome::Applied { at: 64, data: () }),
        (arrived, Outcome::Rejected(Rejection::Unanswered)),
        (parked, Outcome::Rejected(Rejection::Unanswered)),
        (future, Outcome::Rejected(Rejection::Unanswered)),
    ] {
        let Some(ScopedReceipt::Scope(reply_id, receipt)) = sender.receipt() else {
            panic!("whole batch before Closed")
        };
        assert_eq!((reply_id, receipt.seq()), (id, seq));
        assert_eq!(receipt.outcome(), &outcome);
        assert_eq!(receipt.batch().commands.len(), 1);
    }
    assert!(matches!(sender.receipt(), Some(ScopedReceipt::Closed(closed)) if closed == id));
    assert!(sender.receipt().is_none());
    assert!(sender.scope(id).is_none());
    let reopened = sender.open(1).expect("credit and slot returned");
    assert_eq!(reopened.index(), id.index());
    assert_eq!(reopened.generation(), id.generation().wrapping_add(1));
    assert!(inbox.scope(reopened).is_some());
    assert!(sender.scope(reopened).is_some());
    assert!(inbox.scope(id).is_none());
    assert!(sender.close(id).is_err());
    assert_eq!(
        sender
            .scope(reopened)
            .expect("scope")
            .basis(Slot(0), When::Next),
        None
    );
    assert_eq!(sender.scope(reopened).expect("scope").available(), 4);
}

#[kithara::test]
fn stopped_owner_explicitly_drains_and_retires_closing_scopes() {
    let (mut sender, mut inbox) = pair(1, 1);
    let id = sender.open(0).expect("slot");
    scope_send(&mut sender, id, When::Next, 1);
    sender.close(id).expect("reserved");
    sender.publish().expect("live");
    inbox.retire_closing();
    assert!(inbox.scope(id).is_none());
    let Some(ScopedReceipt::Scope(reply_id, receipt)) = sender.receipt() else {
        panic!("unanswered")
    };
    assert_eq!(reply_id, id);
    assert_eq!(receipt.outcome(), &Outcome::Rejected(Rejection::Unanswered));
    assert_eq!(receipt.batch().commands, [1]);
    assert!(matches!(sender.receipt(), Some(ScopedReceipt::Closed(closed)) if closed == id));
}

#[kithara::test]
fn axis_restart_refuses_every_at_and_preserves_other_batches_on_every_level() {
    let (mut sender, mut inbox) = pair(4, 2);
    let ids = [sender.open(0).expect("slot"), sender.open(0).expect("slot")];
    let root_parked = sender.send(When::Next, batch(0)).expect("room");
    let parked = ids.map(|id| scope_send(&mut sender, id, When::Next, 0));
    sender.publish().expect("live");
    inbox.drain();
    inbox.root().next_due(0, 64).expect("Next").defer();
    for id in ids {
        inbox
            .scope(id)
            .expect("scope")
            .next_due(0, 64)
            .expect("Next")
            .defer();
    }
    sender.send(When::At(200), batch(1)).expect("room");
    sender.send(When::Next, batch(2)).expect("room");
    sender.send(When::Deferred, batch(3)).expect("room");
    for id in ids {
        scope_send(&mut sender, id, When::At(200), 1);
        scope_send(&mut sender, id, When::Next, 2);
        scope_send(&mut sender, id, When::Deferred, 3);
    }
    sender.publish().expect("live");
    inbox.refuse_timed("root axis", "scope axis");
    for reason in ["root axis", "scope axis", "scope axis"] {
        let receipt = match sender.receipt().expect("timed refusal") {
            ScopedReceipt::Root(receipt) | ScopedReceipt::Scope(_, receipt) => receipt,
            ScopedReceipt::Closed(_) => panic!("not retiring"),
        };
        assert_eq!(
            receipt.outcome(),
            &Outcome::Rejected(Rejection::Refused(reason))
        );
    }
    assert!(sender.receipt().is_none());
    let mut root = inbox.root();
    assert!(root.is_parked(root_parked));
    assert_eq!(walk(&mut root, 64)[0].2, 2);
    let deferred = root.next_deferred().expect("kept");
    let root_deferred = deferred.seq();
    assert_eq!(deferred.park(), Some(root_deferred));
    root.resume(root_parked, 64, 70)
        .expect("kept parked")
        .apply(());
    root.resume(root_deferred, 64, 80)
        .expect("kept deferred")
        .apply(());
    for (id, seq) in ids.into_iter().zip(parked) {
        let mut scope = inbox.scope(id).expect("scope");
        assert!(scope.is_parked(seq));
        assert_eq!(walk(&mut scope, 64)[0].2, 2);
        scope
            .next_deferred()
            .expect("kept")
            .refuse("executor event");
        scope.resume(seq, 64, 70).expect("kept parked").apply(());
    }
    assert_eq!(std::iter::from_fn(|| sender.receipt()).count(), 9);
}

#[kithara::test]
fn closed_gate_returns_whole_batches_and_publish_reports_closed() {
    let (mut sender, mut inbox) = pair(2, 1);
    let id = sender.open(1).expect("slot");
    let committed = scope_send(&mut sender, id, When::Next, 1);
    sender.publish().expect("live");
    sender.send(When::Next, batch(2)).expect("staged root");
    inbox.drain();
    drop(inbox);
    assert!(sender.publish().is_err());
    let Err(SendError::Closed(returned)) = sender.send(When::Next, batch(3)) else {
        panic!("closed gate")
    };
    assert_eq!(returned.commands, [3]);
    let Err(SendError::Closed(returned)) = sender
        .scope(id)
        .expect("owner slot")
        .send(When::Next, batch(4))
    else {
        panic!("closed scope gate")
    };
    assert_eq!(returned.commands, [4]);
    let Some(ScopedReceipt::Scope(reply_id, receipt)) = sender.receipt() else {
        panic!("only committed commands")
    };
    assert_eq!((reply_id, receipt.seq()), (id, committed));
    assert_eq!(receipt.outcome(), &Outcome::Rejected(Rejection::Unanswered));
    assert!(sender.receipt().is_none(), "teardown sends no Closed");
}

#[kithara::test]
fn dropping_sender_never_commits_an_unpublished_pass() {
    let (mut sender, mut inbox) = pair(1, 1);
    let id = sender.open(0).expect("slot");
    sender.send(When::Next, batch(1)).expect("room");
    scope_send(&mut sender, id, When::Next, 2);
    sender.close(id).expect("reserved");
    drop(sender);
    inbox.drain();
    assert!(inbox.root().next_due(0, 64).is_none());
    let mut scope = inbox.scope(id).expect("not retired");
    assert!(!scope.is_closing());
    assert!(scope.next_due(0, 64).is_none());
}

#[kithara::test]
fn basis_and_eager_stale_are_isolated_to_their_level() {
    let (mut sender, mut inbox) = pair(4, 2);
    let first = sender.open(1).expect("slot");
    let second = sender.open(1).expect("slot");
    let shifting = |command| Batch {
        basis: vec![(Slot(0), None)],
        commands: vec![command, command + 10],
    };
    let root_future = sender.send(When::At(200), shifting(1)).expect("room");
    let first_future = sender
        .scope(first)
        .expect("scope")
        .send(When::At(200), shifting(2))
        .expect("room");
    let second_future = sender
        .scope(second)
        .expect("scope")
        .send(When::At(200), shifting(3))
        .expect("room");
    let root_next = sender.send(When::Next, shifting(4)).expect("room");
    let first_next = sender
        .scope(first)
        .expect("scope")
        .send(When::Next, shifting(5))
        .expect("room");
    assert_eq!(sender.basis(Slot(0), When::At(200)), Some(root_next));
    assert_eq!(
        sender
            .scope(first)
            .expect("scope")
            .basis(Slot(0), When::At(200)),
        Some(first_next)
    );
    assert_eq!(
        sender
            .scope(second)
            .expect("scope")
            .basis(Slot(0), When::Next),
        None
    );
    assert_eq!(
        sender
            .scope(second)
            .expect("scope")
            .basis(Slot(0), When::At(200)),
        Some(second_future)
    );
    sender.publish().expect("live");
    inbox.drain();
    assert!(sender.receipt().is_none(), "drain judges no level");
    inbox.root().next_due(64, 64).expect("root Next").apply(());
    inbox
        .scope(first)
        .expect("scope")
        .next_due(64, 64)
        .expect("scope Next")
        .apply(());
    assert!(
        inbox
            .scope(second)
            .expect("scope")
            .next_due(64, 64)
            .is_none()
    );
    for (expected_id, seq, command, outcome) in [
        (None, root_next, 4, Outcome::Applied { at: 64, data: () }),
        (None, root_future, 1, Outcome::Rejected(Rejection::Stale)),
        (
            Some(first),
            first_next,
            5,
            Outcome::Applied { at: 64, data: () },
        ),
        (
            Some(first),
            first_future,
            2,
            Outcome::Rejected(Rejection::Stale),
        ),
    ] {
        let (id, receipt) = match sender.receipt().expect("answer") {
            ScopedReceipt::Root(receipt) => (None, receipt),
            ScopedReceipt::Scope(id, receipt) => (Some(id), receipt),
            ScopedReceipt::Closed(_) => panic!("no close"),
        };
        assert_eq!((id, receipt.seq()), (expected_id, seq));
        assert_eq!(receipt.outcome(), &outcome);
        assert_eq!(receipt.batch().commands, [command, command + 10]);
    }
    assert!(sender.receipt().is_none(), "one verdict per batch");
    inbox
        .scope(second)
        .expect("scope")
        .next_due(200, 64)
        .expect("unaffected future")
        .apply(());
    assert!(
        matches!(sender.receipt(), Some(ScopedReceipt::Scope(id, receipt))
        if id == second && receipt.seq() == second_future && receipt.outcome() == &Outcome::Applied { at: 200, data: () })
    );
    assert!(sender.receipt().is_none());
}
