use std::convert::Infallible;
#[cfg(panic = "unwind")]
use std::panic::{AssertUnwindSafe, catch_unwind};

use kithara_platform::{
    thread,
    time::{Duration, timeout},
    tokio::sync::oneshot,
};
use kithara_test_utils::kithara;
#[cfg(panic = "unwind")]
use ringbuf::traits::Consumer;
use ringbuf::traits::Producer;

#[cfg(panic = "unwind")]
use super::{Item, ScopeId, ScopeReply};
use super::{ScopedConfig, scoped_channel};
#[cfg(panic = "unwind")]
use crate::channel::sender::Sent;
use crate::{Batch, Outcome, Protocol, Rejection, Seq, When};

#[kithara::hang_watchdog(timeout = Duration::from_secs(1), ctx = String)]
fn await_plain_answers(sender: &mut crate::Sender<Test>, expected: &[Seq]) {
    let mut answered = Vec::new();
    while answered.len() < expected.len() {
        hang_tick!(format!(
            "inbox teardown answered {} of {} admitted batches",
            answered.len(),
            expected.len()
        ));
        for receipt in sender.receipts() {
            assert_eq!(receipt.outcome(), &Outcome::Rejected(Rejection::Unanswered));
            assert!(
                !answered.contains(&receipt.seq()),
                "one answer per admission"
            );
            answered.push(receipt.seq());
            hang_reset!();
        }
        if answered.len() < expected.len() {
            hang_park!(
                |remaining: Duration| thread::park_timeout(remaining.min(Duration::from_millis(1))),
                format!(
                    "inbox teardown answered {} of {} admitted batches",
                    answered.len(),
                    expected.len()
                )
            );
        }
    }
    assert_eq!(answered, expected);
    assert_eq!(sender.receipts().count(), 0, "no duplicate answer");
}

#[kithara::test(native, tokio, browser)]
#[kithara::hang_watchdog(timeout = Duration::from_secs(1), ctx = String)]
async fn sends_racing_inbox_drop_are_answered_exactly_once() {
    let _real_io = kithara_platform::flash::real_io();
    let (sender, inbox) = crate::channel::<Test>(crate::ChannelConfig::builder().build());
    let (started, receiving) = oneshot::channel();
    let (done, completed) = oneshot::channel();
    #[kithara::hang_watchdog(timeout = Duration::from_secs(1), ctx = String)]
    fn hammer_sends(
        mut sender: crate::Sender<Test>,
        started: oneshot::Sender<()>,
    ) -> (crate::Sender<Test>, Vec<Seq>) {
        let mut started = Some(started);
        let mut admitted = Vec::new();
        loop {
            hang_tick!(format!(
                "sender awaits inbox closure after {} admissions",
                admitted.len()
            ));
            match sender.send(
                When::Next,
                Batch {
                    basis: Vec::new(),
                    commands: vec![0],
                },
            ) {
                Ok(seq) => {
                    admitted.push(seq);
                    hang_reset!();
                    if let Some(started) = started.take() {
                        started.send(()).expect("closer is waiting");
                    }
                }
                Err(crate::SendError::Closed(_)) => break,
                Err(crate::SendError::Full(_)) => {
                    hang_park!(
                        |remaining: Duration| thread::park_timeout(
                            remaining.min(Duration::from_millis(1))
                        ),
                        "sender awaits inbox closure".to_owned()
                    );
                }
                Err(crate::SendError::Target(_)) => panic!("empty basis is admitted"),
            }
        }
        (sender, admitted)
    }
    drop(thread::spawn(move || {
        assert!(done.send(hammer_sends(sender, started)).is_ok());
    }));
    hang_tick!("sender has not reached its first admission".to_owned());
    timeout(__hang_detector.remaining(), receiving)
        .await
        .expect("sender reaches its first admission within the watchdog budget")
        .expect("sender reaches its first admission");
    hang_reset!();
    drop(inbox);
    hang_tick!("sender has not left the closed channel".to_owned());
    let (mut sender, admitted) = timeout(__hang_detector.remaining(), completed)
        .await
        .expect("sender leaves the closed channel within the watchdog budget")
        .expect("sender must not panic");
    assert!(!admitted.is_empty());
    await_plain_answers(&mut sender, &admitted);
}

#[cfg(panic = "unwind")]
#[kithara::test]
fn lost_answer_negative_control_trips_the_watchdog() {
    let _real_io = kithara_platform::flash::real_io();
    let (mut sender, inbox) = crate::channel::<Test>(crate::ChannelConfig::builder().build());
    let seq = sender
        .send(
            When::Next,
            Batch {
                basis: Vec::new(),
                commands: vec![1],
            },
        )
        .expect("admitted");
    drop(inbox);
    assert_eq!(sender.receipts().count(), 1);
    let failure = catch_unwind(AssertUnwindSafe(|| {
        await_plain_answers(&mut sender, &[seq]);
    }));
    let panic = failure.expect_err("lost answer must fail instead of timing out the test runner");
    let reason = panic.downcast_ref::<String>().expect("watchdog reason");
    assert!(
        reason.contains("inbox teardown answered 0 of 1 admitted batches"),
        "{reason}"
    );
}

#[kithara::test]
fn a_publication_admitted_before_drop_recovers_both_levels() {
    use crate::Port;
    let (mut sender, inbox) = scoped_channel::<Test, Test>(ScopedConfig::builder().build());
    let id = sender.open(0).expect("slot");
    let root = sender
        .send(
            When::Next,
            Batch {
                basis: Vec::new(),
                commands: vec![1],
            },
        )
        .expect("root");
    let scope = sender
        .scope(id)
        .expect("scope")
        .send(
            When::Next,
            Batch {
                basis: Vec::new(),
                commands: vec![2],
            },
        )
        .expect("scope admission");
    assert!(sender.gate.enter());
    drop(inbox);
    assert_eq!(sender.commands.push_iter(sender.staged.drain(..)), 2);
    let super::ScopedReceipt::Root(receipt) = sender.receipt().expect("root answer") else {
        panic!("root first")
    };
    assert_eq!(receipt.seq(), root);
    assert_eq!(receipt.outcome(), &Outcome::Rejected(Rejection::Unanswered));
    let super::ScopedReceipt::Scope(answered_id, receipt) = sender.receipt().expect("scope answer")
    else {
        panic!("scope next")
    };
    assert_eq!(answered_id, id);
    assert_eq!(receipt.seq(), scope);
    assert_eq!(receipt.outcome(), &Outcome::Rejected(Rejection::Unanswered));
    assert!(sender.receipt().is_none());
}

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

#[cfg(panic = "unwind")]
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
