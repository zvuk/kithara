#![cfg(not(target_arch = "wasm32"))]
#![forbid(unsafe_code)]

use std::num::NonZeroUsize;

use assert_no_alloc::{AllocDisabler, assert_no_alloc};
use kithara_command::{
    Batch, ChannelConfig, Outcome, Protocol, Rejection, Seq, Target, When, channel,
};
use kithara_test_utils::kithara;

#[global_allocator]
static ALLOCATOR: AllocDisabler = AllocDisabler;

const CAPACITY: NonZeroUsize = NonZeroUsize::MIN.saturating_add(15);
const BLOCK: usize = 64;
type Parts = (Outcome<Test>, Batch<Test>);

#[derive(Debug, PartialEq, Eq)]
enum Test {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Slot(usize);

impl Target for Slot {
    fn index(self) -> usize {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Frame(u64);

impl Protocol for Test {
    type Applied = ();
    type Clock = Frame;
    type Command = u32;
    type Refusal = &'static str;
    type Target = Slot;

    fn frames_since(at: Frame, start: Frame) -> Option<u64> {
        at.0.checked_sub(start.0)
    }
}

fn batch(command: u32, basis: &[(Slot, Option<Seq>)]) -> Batch<Test> {
    Batch {
        basis: basis.to_vec(),
        commands: vec![command],
    }
}

#[kithara::test(native)]
fn plain_commit_edit_and_complete_never_allocate_or_drop_batches() {
    let (mut sender, mut inbox) = channel::<Test>(
        ChannelConfig::builder()
            .capacity(CAPACITY)
            .targets(1)
            .build(),
    );
    let mut sequences = [None; CAPACITY.get()];
    for sequence in &mut sequences {
        *sequence = Some(
            sender
                .send(When::At(Frame(80)), batch(1, &[]))
                .expect("room"),
        );
    }
    assert_no_alloc(|| {
        inbox.drain();
        for seq in sequences.into_iter().flatten() {
            assert_eq!(inbox.next_due(Frame(64), BLOCK).expect("due").commit(), seq);
        }
        for seq in sequences.into_iter().flatten().rev() {
            inbox.committed_mut(seq).expect("held commands")[0] = 99;
            assert!(inbox.complete(seq, ()));
        }
    });
    for receipt in sender.receipts() {
        assert_eq!(
            receipt.outcome(),
            &Outcome::Applied {
                at: Frame(80),
                data: ()
            }
        );
        assert_eq!(receipt.batch().commands, [99]);
    }
    assert_eq!(sender.available(), CAPACITY.get());
}

#[kithara::test(native)]
fn scoped_commit_edit_complete_and_retire_never_allocate_or_drop_batches() {
    use kithara_command::{Port, ScopedConfig, ScopedReceipt, scoped_channel};

    let config = ChannelConfig::builder()
        .capacity(CAPACITY)
        .targets(1)
        .build();
    let (mut sender, mut inbox) = scoped_channel::<Test, Test>(
        ScopedConfig::builder()
            .root(config)
            .scope(config)
            .scopes(std::num::NonZeroU16::MIN)
            .build(),
    );
    let id = sender.open(1).expect("scope");
    let root = sender.send(When::Next, batch(1, &[])).expect("root room");
    let mut sequences = [None; CAPACITY.get()];
    for sequence in &mut sequences {
        *sequence = Some(
            sender
                .scope(id)
                .expect("scope")
                .send(When::At(Frame(80)), batch(1, &[]))
                .expect("room"),
        );
    }
    sender.close(id).expect("reserved");
    sender.publish().expect("live");
    assert_no_alloc(|| {
        inbox.drain();
        let mut level = inbox.root();
        assert_eq!(
            level.next_due(Frame(64), BLOCK).expect("root").commit(),
            root
        );
        level.committed_mut(root).expect("root commands")[0] = 99;
        assert!(level.complete(root, ()));
        let mut level = inbox.scope(id).expect("scope");
        for seq in sequences.into_iter().flatten() {
            assert_eq!(level.next_due(Frame(64), BLOCK).expect("due").commit(), seq);
            level.committed_mut(seq).expect("held commands")[0] = 99;
        }
        for seq in sequences
            .into_iter()
            .flatten()
            .rev()
            .take(CAPACITY.get() / 2)
        {
            assert!(level.complete(seq, ()));
        }
        level.retire();
    });
    let receipts: Vec<_> = std::iter::from_fn(|| sender.receipt()).collect();
    assert_eq!(receipts.len(), CAPACITY.get() + 2);
    for receipt in &receipts {
        match receipt {
            ScopedReceipt::Root(receipt) | ScopedReceipt::Scope(_, receipt) => {
                assert_eq!(receipt.batch().commands, [99]);
                assert!(matches!(
                    receipt.outcome(),
                    Outcome::Applied { .. } | Outcome::Rejected(Rejection::Unanswered)
                ));
            }
            ScopedReceipt::Closed(closed) => assert_eq!(*closed, id),
        }
    }
    assert!(matches!(receipts.last(), Some(ScopedReceipt::Closed(_))));
}

#[kithara::test(native)]
fn draining_judging_and_answering_never_allocate() {
    const START: Frame = Frame(64);
    const REFUSED: u32 = 4;
    const UNANSWERED: u32 = 6;

    let config = ChannelConfig::builder()
        .capacity(CAPACITY)
        .targets(2)
        .build();
    let (mut sender, mut inbox) = channel::<Test>(config);
    let marked = [
        (When::Next, batch(1, &[(Slot(0), None)])),
        (When::At(Frame(0)), batch(2, &[])),
        (When::At(Frame(80)), batch(3, &[(Slot(0), None)])),
        (When::At(Frame(90)), batch(REFUSED, &[(Slot(1), None)])),
        (When::At(Frame(1000)), batch(5, &[])),
        (When::At(Frame(100)), batch(UNANSWERED, &[])),
    ];
    let fillers = (10..).map(|command| (When::Next, batch(command, &[])));
    for (when, batch) in marked.into_iter().chain(fillers).take(CAPACITY.get()) {
        sender.send(when, batch).expect("the channel has room");
    }

    assert_no_alloc(|| {
        inbox.drain();
        while let Some(due) = inbox.next_due(START, BLOCK) {
            if due.commands().contains(&REFUSED) {
                due.refuse("busy");
            } else if due.commands().contains(&UNANSWERED) {
                drop(due);
            } else {
                due.apply(());
            }
        }
    });

    let outcomes: Vec<Outcome<Test>> = sender
        .receipts()
        .map(|receipt| Parts::from(receipt).0)
        .collect();
    let applied = outcomes
        .iter()
        .filter(|outcome| matches!(outcome, Outcome::Applied { .. }))
        .count();
    assert_eq!(outcomes.len(), CAPACITY.get() - 1);
    assert_eq!(applied, CAPACITY.get() - 5);
    assert!(outcomes.contains(&Outcome::Rejected(Rejection::Late)));
    assert!(outcomes.contains(&Outcome::Rejected(Rejection::Stale)));
    assert!(outcomes.contains(&Outcome::Rejected(Rejection::Refused("busy"))));
    assert!(outcomes.contains(&Outcome::Rejected(Rejection::Unanswered)));
}

#[kithara::test(native)]
fn deferred_resume_and_eager_scans_never_allocate_or_drop_batches() {
    let (mut sender, mut inbox) = channel::<Test>(
        ChannelConfig::builder()
            .capacity(CAPACITY)
            .targets(1)
            .build(),
    );
    let parked_due = sender
        .send(When::Next, batch(1, &[(Slot(0), None)]))
        .expect("room");
    let parked_arrival = sender
        .send(When::Deferred, batch(2, &[(Slot(0), None)]))
        .expect("room");
    sender
        .send(When::Deferred, batch(3, &[(Slot(0), None)]))
        .expect("room");
    sender
        .send(When::At(Frame(1000)), batch(4, &[(Slot(0), None)]))
        .expect("room");
    sender
        .send(When::Next, batch(5, &[(Slot(0), None)]))
        .expect("room");
    let resumed = sender.send(When::Next, batch(6, &[])).expect("room");
    sender.send(When::Deferred, batch(7, &[])).expect("room");
    sender.send(When::Deferred, batch(8, &[])).expect("room");

    assert_no_alloc(|| {
        inbox.drain();
        assert_eq!(
            inbox.next_due(Frame(64), BLOCK).expect("due").defer(),
            parked_due
        );
        assert_eq!(
            inbox.next_deferred().expect("arrival").park(),
            Some(parked_arrival)
        );
        inbox.next_due(Frame(64), BLOCK).expect("shift").apply(());
        assert!(!inbox.is_parked(parked_due));
        assert!(!inbox.is_parked(parked_arrival));
        inbox.next_due(Frame(64), BLOCK).expect("due").defer();
        let due = inbox
            .resume(resumed, Frame(128), Frame(150))
            .expect("parked");
        assert_eq!(due.offset(), 22);
        due.refuse("executor event");
        inbox
            .next_deferred()
            .expect("arrival")
            .refuse("invalid deferral");
        drop(inbox.next_deferred().expect("arrival"));
        assert!(inbox.next_due(Frame(64), BLOCK).is_none());
        assert!(inbox.next_deferred().is_none());
    });
    let receipts: Vec<_> = sender.receipts().collect();
    assert_eq!(receipts.len(), 8);
    assert_eq!(
        receipts
            .iter()
            .filter(|receipt| matches!(receipt.outcome(), Outcome::Rejected(Rejection::Stale)))
            .count(),
        4
    );
    assert!(
        receipts
            .iter()
            .all(|receipt| receipt.batch().commands.len() == 1)
    );
}

#[kithara::test(native)]
fn scoped_publish_walk_refuse_and_retire_never_allocate_or_drop_batches() {
    use kithara_command::{Port, ScopedConfig, ScopedReceipt, scoped_channel};

    let level = ChannelConfig::builder()
        .capacity(CAPACITY)
        .targets(1)
        .build();
    let (mut sender, mut inbox) = scoped_channel::<Test, Test>(
        ScopedConfig::builder()
            .root(level)
            .scope(level)
            .scopes(std::num::NonZeroU16::MIN.saturating_add(1))
            .build(),
    );
    let first = sender.open(1).expect("slot");
    let second = sender.open(1).expect("slot");
    let root = sender.send(When::Next, batch(1, &[])).expect("room");
    sender
        .send(When::At(Frame(1000)), batch(2, &[]))
        .expect("room");
    for id in [first, second] {
        let mut port = sender.scope(id).expect("scope");
        port.send(When::Deferred, batch(3, &[(Slot(0), None)]))
            .expect("room");
        port.send(When::Next, batch(4, &[(Slot(0), None)]))
            .expect("room");
        port.send(When::At(Frame(1000)), batch(5, &[(Slot(0), None)]))
            .expect("room");
        port.send(When::Deferred, batch(6, &[])).expect("room");
    }
    sender.close(first).expect("reserved");
    sender.close(second).expect("reserved");

    assert_no_alloc(|| {
        sender.publish().expect("live");
        inbox.drain();
        let mut level = inbox.root();
        assert_eq!(
            level.next_due(Frame(64), BLOCK).expect("Next").defer(),
            root
        );
        level
            .resume(root, Frame(128), Frame(150))
            .expect("parked")
            .apply(());
        let mut level = inbox.scope(first).expect("scope");
        let parked = level
            .next_deferred()
            .expect("arrival")
            .park()
            .expect("current");
        level.next_due(Frame(64), BLOCK).expect("shift").apply(());
        assert!(!level.is_parked(parked));
        level
            .next_deferred()
            .expect("arrival")
            .refuse("invalid deferral");
        level.retire();
        let mut level = inbox.scope(second).expect("scope");
        level.next_due(Frame(64), BLOCK).expect("Next").defer();
        level
            .next_deferred()
            .expect("arrival")
            .park()
            .expect("current");
        inbox.refuse_timed("root axis", "scope axis");
        inbox.retire_closing();
        assert!(inbox.scope(first).is_none());
        assert!(inbox.scope(second).is_none());
    });
    let receipts: Vec<_> = std::iter::from_fn(|| sender.receipt()).collect();
    assert_eq!(receipts.len(), 12);
    assert_eq!(
        receipts
            .iter()
            .filter(|receipt| matches!(receipt, ScopedReceipt::Closed(_)))
            .count(),
        2
    );
}
