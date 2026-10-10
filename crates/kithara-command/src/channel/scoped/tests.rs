use std::{
    convert::Infallible,
    panic::{AssertUnwindSafe, catch_unwind},
};

use kithara_test_utils::kithara;
use ringbuf::traits::{Consumer, Producer};

use super::{Item, ScopeId, ScopeReply, ScopedConfig, scoped_channel};
use crate::{Batch, Outcome, Protocol, Rejection, Seq, When, channel::sender::Sent};

#[derive(Debug, PartialEq, Eq)]
enum Test {}

impl Protocol for Test {
    type Applied = ();
    type Clock = u64;
    type Command = u32;
    type Refusal = ();
    type Target = Infallible;

    fn frames_since(at: u64, start: u64) -> Option<u64> {
        at.checked_sub(start)
    }
}

#[kithara::test]
fn a_foreign_generation_is_answered_whole_and_never_routed() {
    let (mut sender, mut inbox) = scoped_channel::<Test, Test>(ScopedConfig::builder().build());
    let id = sender.open(0).expect("slot");
    let foreign = ScopeId {
        index: id.index,
        generation: id.generation.wrapping_add(1),
    };
    assert!(
        sender
            .commands
            .try_push(Item::Scope {
                id: foreign,
                sent: Sent {
                    seq: Seq::FIRST,
                    when: When::Next,
                    batch: Batch {
                        basis: Vec::new(),
                        commands: vec![41, 42]
                    }
                },
            })
            .is_ok()
    );
    sender.publish().expect("live");
    let drained = catch_unwind(AssertUnwindSafe(|| inbox.drain()));
    assert_eq!(
        drained.is_err(),
        cfg!(debug_assertions),
        "bad generation is an internal error"
    );
    assert!(
        inbox
            .scope(id)
            .expect("current generation")
            .next_due(0, 64)
            .is_none()
    );
    let Some(ScopeReply::Receipt {
        id: reply_id,
        receipt,
    }) = sender.scope_receipts.try_pop()
    else {
        panic!("foreign item is returned, not dropped")
    };
    assert_eq!(reply_id, foreign);
    assert_eq!(receipt.seq(), Seq::FIRST);
    assert_eq!(receipt.outcome(), &Outcome::Rejected(Rejection::Unanswered));
    assert_eq!(receipt.batch().commands, [41, 42]);
    assert!(
        sender.scope_receipts.try_pop().is_none(),
        "exactly one verdict"
    );
}

#[kithara::test]
fn a_closed_gate_spends_neither_a_number_nor_a_credit() {
    use crate::{Port, SendError};

    let (mut sender, inbox) = scoped_channel::<Test, Test>(ScopedConfig::builder().build());
    let id = sender.open(0).expect("slot");
    let number = sender.next;
    let root_credits = sender.available();
    let scope_credits = sender.scope(id).expect("scope").available();
    drop(inbox);
    for scoped in [false, true] {
        let batch = Batch {
            basis: Vec::new(),
            commands: vec![41, 42],
        };
        let result = if scoped {
            sender
                .scope(id)
                .expect("owner scope")
                .send(When::Next, batch)
        } else {
            sender.send(When::Next, batch)
        };
        let Err(SendError::Closed(batch)) = result else {
            panic!("closed gate")
        };
        assert_eq!(batch.commands, [41, 42]);
        assert!(batch.basis.is_empty());
        assert_eq!(sender.next, number);
    }
    assert_eq!(sender.available(), root_credits);
    assert_eq!(
        sender.scope(id).expect("owner scope").available(),
        scope_credits
    );
    assert!(sender.publish().is_err());
}

#[kithara::test]
fn a_scoped_holder_rearms_on_receipt_and_release_stops_waking() {
    use crate::Port;

    let (mut sender, mut inbox) = scoped_channel::<Test, Test>(ScopedConfig::builder().build());
    let id = sender.open(0).expect("slot");
    let (waker, wakes) = crate::wakes::waker();
    sender.hold(waker);
    sender
        .send(
            When::Next,
            Batch {
                basis: Vec::new(),
                commands: vec![1],
            },
        )
        .expect("room");
    sender
        .scope(id)
        .expect("scope")
        .send(
            When::Next,
            Batch {
                basis: Vec::new(),
                commands: vec![2],
            },
        )
        .expect("room");
    sender.close(id).expect("reserved");
    sender.publish().expect("live");
    assert_eq!(wakes.count(), 0);
    inbox.drain();
    inbox.root().next_due(0, 64).expect("root").apply(());
    assert_eq!(wakes.count(), 1);
    assert!(sender.receipt().is_some());
    inbox
        .scope(id)
        .expect("scope")
        .next_due(0, 64)
        .expect("scope")
        .apply(());
    assert_eq!(wakes.count(), 2);
    assert!(sender.receipt().is_some());
    sender.release();
    inbox.scope(id).expect("closing").retire();
    assert_eq!(wakes.count(), 2);
    assert!(matches!(sender.receipt(), Some(super::ScopedReceipt::Closed(closed)) if closed == id));
}
