use std::{
    mem,
    num::NonZeroUsize,
    ops::{Deref, DerefMut, Range},
    task::{Context, Poll},
};

use kithara_platform::{thread, tokio::sync::oneshot};
use kithara_test_utils::kithara;

use super::{SendError, Sender, Step, channel};
use crate::{
    ChannelConfig, Inbox,
    protocol::{Batch, Protocol, Seq, Target, When},
    receipt::{Outcome, Rejection},
    wakes::waker,
};

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

fn pair(capacity: usize, targets: usize) -> (Sender<Test>, Inbox<Test>) {
    let capacity = NonZeroUsize::new(capacity).expect("a test channel holds a batch");
    channel(
        ChannelConfig::builder()
            .capacity(capacity)
            .targets(targets)
            .build(),
    )
}

fn batch(command: u32, basis: &[(Slot, Option<Seq>)]) -> Batch<Test> {
    Batch {
        basis: basis.to_vec(),
        commands: vec![command],
    }
}

fn send(sender: &mut Sender<Test>, when: When<Frame>, batch: Batch<Test>) -> Seq {
    sender.send(when, batch).expect("the test channel has room")
}

fn run_block(inbox: &mut Inbox<Test>, start: u64, frames: usize) -> Vec<(usize, u32)> {
    inbox.drain();
    let mut applied = Vec::new();
    while let Some(due) = inbox.next_due(Frame(start), frames) {
        let offset = due.offset();
        applied.extend(due.commands().iter().map(|&command| (offset, command)));
        due.apply(());
    }
    applied
}

fn outcomes(sender: &mut Sender<Test>) -> Vec<(Seq, Outcome<Test>)> {
    sender
        .receipts()
        .map(|receipt| {
            let seq = receipt.seq();
            let (outcome, _): Parts = receipt.into();
            (seq, outcome)
        })
        .collect()
}

fn applied(at: u64) -> Outcome<Test> {
    Outcome::Applied {
        at: Frame(at),
        data: (),
    }
}

#[kithara::test]
fn batches_run_in_time_order_then_send_order() {
    let (mut sender, mut inbox) = pair(8, 0);
    send(&mut sender, When::At(Frame(40)), batch(1, &[]));
    send(&mut sender, When::At(Frame(10)), batch(2, &[]));
    send(&mut sender, When::At(Frame(40)), batch(3, &[]));
    send(&mut sender, When::Next, batch(4, &[]));

    assert_eq!(
        run_block(&mut inbox, 0, BLOCK),
        [(0, 4), (10, 2), (40, 1), (40, 3)]
    );
}

#[kithara::test]
fn the_last_frame_is_due_and_the_block_end_waits() {
    let (mut sender, mut inbox) = pair(8, 0);
    send(&mut sender, When::At(Frame(63)), batch(1, &[]));
    send(&mut sender, When::At(Frame(64)), batch(2, &[]));

    assert_eq!(run_block(&mut inbox, 0, BLOCK), [(63, 1)]);
    assert_eq!(run_block(&mut inbox, 64, BLOCK), [(0, 2)]);
}

#[kithara::test]
fn a_moment_at_the_end_of_the_clock_waits() {
    let (mut sender, mut inbox) = pair(8, 0);
    send(&mut sender, When::At(Frame(u64::MAX)), batch(1, &[]));

    assert!(run_block(&mut inbox, 0, BLOCK).is_empty());
    assert!(outcomes(&mut sender).is_empty());
}

#[kithara::test]
fn next_applies_at_the_block_start() {
    let (mut sender, mut inbox) = pair(8, 0);
    let seq = send(&mut sender, When::Next, batch(1, &[]));

    assert_eq!(run_block(&mut inbox, 128, BLOCK), [(0, 1)]);
    assert_eq!(outcomes(&mut sender), [(seq, applied(128))]);
}

#[kithara::test]
fn the_next_moment_bounds_a_block_the_executor_sizes() {
    let (mut sender, mut inbox) = pair(8, 0);
    assert_eq!(inbox.frames_until_due(Frame(0)), None);
    send(&mut sender, When::At(Frame(40)), batch(1, &[]));
    send(&mut sender, When::At(Frame(10)), batch(2, &[]));
    inbox.drain();

    assert_eq!(inbox.frames_until_due(Frame(4)), Some(6));
    assert_eq!(inbox.frames_until_due(Frame(12)), Some(0));

    send(&mut sender, When::Next, batch(3, &[]));
    inbox.drain();
    assert_eq!(inbox.frames_until_due(Frame(4)), Some(0));
}

/// What an executor saw while it rendered one block through its inbox.
#[derive(Debug, PartialEq, Eq)]
enum Seen {
    Run(Range<usize>),
    Due(usize, u32),
}

fn render(inbox: &mut Inbox<Test>, start: u64, frames: usize) -> Vec<Seen> {
    let mut seen = Vec::new();
    inbox.run_block(Frame(start), frames, |step| match step {
        Step::Run(range) => seen.push(Seen::Run(range)),
        Step::Due(due) => {
            seen.extend(
                due.commands()
                    .iter()
                    .map(|&command| Seen::Due(due.offset(), command)),
            );
            due.apply(());
        }
    });
    seen
}

#[kithara::test]
fn a_block_renders_up_to_each_due_batch_and_applies_it_at_its_frame() {
    let (mut sender, mut inbox) = pair(8, 0);
    send(&mut sender, When::At(Frame(140)), batch(1, &[]));
    send(&mut sender, When::At(Frame(110)), batch(2, &[]));
    send(&mut sender, When::Next, batch(3, &[]));

    assert_eq!(
        render(&mut inbox, 100, BLOCK),
        [
            Seen::Due(0, 3),
            Seen::Run(0..10),
            Seen::Due(10, 2),
            Seen::Run(10..40),
            Seen::Due(40, 1),
            Seen::Run(40..BLOCK),
        ]
    );
    assert_eq!(outcomes(&mut sender).len(), 3, "every due batch answered");
}

#[kithara::test]
fn a_late_batch_ahead_leaves_the_next_one_at_its_own_frame() {
    let (mut sender, mut inbox) = pair(8, 0);
    let late = send(&mut sender, When::At(Frame(10)), batch(1, &[]));
    let due = send(&mut sender, When::At(Frame(140)), batch(2, &[]));

    assert_eq!(
        render(&mut inbox, 100, BLOCK),
        [Seen::Run(0..40), Seen::Due(40, 2), Seen::Run(40..BLOCK)]
    );
    assert_eq!(
        outcomes(&mut sender),
        [
            (late, Outcome::Rejected(Rejection::Late)),
            (due, applied(140))
        ]
    );
}

#[kithara::test]
fn a_block_before_the_next_moment_renders_whole_and_leaves_it_waiting() {
    let (mut sender, mut inbox) = pair(8, 0);
    let end = Frame(u64::try_from(BLOCK).expect("the block fits the clock"));
    send(&mut sender, When::At(end), batch(1, &[]));

    assert_eq!(render(&mut inbox, 0, BLOCK), [Seen::Run(0..BLOCK)]);
    assert!(outcomes(&mut sender).is_empty(), "the batch still waits");
    assert_eq!(
        render(&mut inbox, 0, 0),
        [],
        "an empty block renders nothing"
    );
    assert_eq!(
        render(&mut inbox, end.0, BLOCK),
        [Seen::Due(0, 1), Seen::Run(0..BLOCK)]
    );
}

#[kithara::test]
fn a_moment_before_the_block_is_late_and_returns_whole() {
    let (mut sender, mut inbox) = pair(8, 1);
    let late = send(
        &mut sender,
        When::At(Frame(10)),
        batch(7, &[(Slot(0), None)]),
    );

    assert!(run_block(&mut inbox, 64, BLOCK).is_empty());
    let receipt = sender.receipts().next().expect("the late batch comes back");
    assert_eq!(receipt.seq(), late);
    assert_eq!(receipt.outcome(), &Outcome::Rejected(Rejection::Late));
    let (_, returned): Parts = receipt.into();
    assert_eq!(returned.basis, [(Slot(0), None)]);
    assert_eq!(returned.commands, [7]);

    send(&mut sender, When::Next, batch(8, &[(Slot(0), None)]));
    assert_eq!(run_block(&mut inbox, 128, BLOCK), [(0, 8)]);
}

#[kithara::test]
fn a_skipped_block_returns_its_batches_late_in_time_order() {
    let (mut sender, mut inbox) = pair(8, 0);
    let second = send(&mut sender, When::At(Frame(70)), batch(2, &[]));
    let first = send(&mut sender, When::At(Frame(65)), batch(1, &[]));

    assert!(run_block(&mut inbox, 128, BLOCK).is_empty());
    assert_eq!(
        outcomes(&mut sender),
        [
            (first, Outcome::Rejected(Rejection::Late)),
            (second, Outcome::Rejected(Rejection::Late)),
        ]
    );
}

#[kithara::test]
fn a_new_axis_refuses_every_timed_batch_and_keeps_the_next_ones() {
    let (mut sender, mut inbox) = pair(3, 0);
    let later = send(&mut sender, When::At(Frame(200)), batch(1, &[]));
    let earlier = send(&mut sender, When::At(Frame(100)), batch(2, &[]));
    let next = send(&mut sender, When::Next, batch(3, &[]));

    inbox.refuse_timed("new axis");

    let refused = || Outcome::Rejected(Rejection::Refused("new axis"));
    assert_eq!(
        outcomes(&mut sender),
        [(earlier, refused()), (later, refused())]
    );
    send(&mut sender, When::At(Frame(150)), batch(4, &[]));
    send(&mut sender, When::At(Frame(160)), batch(5, &[]));
    assert_eq!(
        run_block(&mut inbox, 128, BLOCK),
        [(0, 3), (22, 4), (32, 5)]
    );
    assert_eq!(outcomes(&mut sender)[0], (next, applied(128)));
}

#[kithara::test]
fn a_full_channel_returns_the_batch_whole() {
    let (mut sender, _inbox) = pair(1, 0);
    send(&mut sender, When::Next, batch(1, &[]));

    let Err(SendError::Full(returned)) = sender.send(When::Next, batch(2, &[])) else {
        panic!("a second batch overflows a channel of one");
    };
    assert_eq!(returned.commands, [2]);
}

#[kithara::test]
fn credits_return_only_with_receipts() {
    let (mut sender, mut inbox) = pair(1, 0);
    send(&mut sender, When::Next, batch(1, &[]));
    inbox.drain();
    assert!(matches!(
        sender.send(When::Next, batch(2, &[])),
        Err(SendError::Full(_))
    ));

    assert_eq!(run_block(&mut inbox, 0, BLOCK), [(0, 1)]);
    assert!(matches!(
        sender.send(When::Next, batch(3, &[])),
        Err(SendError::Full(_))
    ));

    assert_eq!(outcomes(&mut sender).len(), 1);
    assert_eq!(send(&mut sender, When::Next, batch(4, &[])).get(), 2);
}

#[kithara::test]
fn an_unknown_target_is_refused_before_numbering() {
    let (mut sender, _inbox) = pair(8, 1);

    let Err(SendError::Target(returned)) = sender.send(When::Next, batch(1, &[(Slot(1), None)]))
    else {
        panic!("slot 1 is outside a channel of one target");
    };
    assert_eq!(returned.commands, [1]);
    assert_eq!(
        send(&mut sender, When::Next, batch(2, &[(Slot(0), None)])).get(),
        1
    );
}

#[kithara::test]
fn receipts_follow_execution_order() {
    let (mut sender, mut inbox) = pair(8, 0);
    let a = send(&mut sender, When::At(Frame(30)), batch(1, &[]));
    let b = send(&mut sender, When::At(Frame(10)), batch(2, &[]));
    let c = send(&mut sender, When::At(Frame(20)), batch(3, &[]));

    run_block(&mut inbox, 0, BLOCK);
    let order: Vec<Seq> = outcomes(&mut sender)
        .into_iter()
        .map(|(seq, _)| seq)
        .collect();
    assert_eq!(order, [b, c, a]);
}

#[kithara::test]
fn a_refusal_leaves_the_ledger_untouched() {
    let (mut sender, mut inbox) = pair(8, 1);
    let refused = send(&mut sender, When::Next, batch(1, &[(Slot(0), None)]));
    inbox.drain();
    let due = inbox.next_due(Frame(0), BLOCK).expect("the batch is due");
    due.refuse("busy");

    let retry = send(&mut sender, When::Next, batch(2, &[(Slot(0), None)]));
    assert_eq!(run_block(&mut inbox, 64, BLOCK), [(0, 2)]);
    assert_eq!(
        outcomes(&mut sender),
        [
            (refused, Outcome::Rejected(Rejection::Refused("busy"))),
            (retry, applied(64)),
        ]
    );
}

#[kithara::test]
fn the_executor_returns_resources_inside_the_batch() {
    let (mut sender, mut inbox) = pair(8, 0);
    send(&mut sender, When::Next, batch(7, &[]));
    inbox.drain();

    let mut due = inbox.next_due(Frame(0), BLOCK).expect("the batch is due");
    let taken = mem::replace(&mut due.commands_mut()[0], 99);
    due.apply(());

    assert_eq!(taken, 7);
    let receipt = sender.receipts().next().expect("the receipt arrives");
    let (_, returned): Parts = receipt.into();
    assert_eq!(returned.commands, [99]);
}

#[kithara::test]
fn the_executor_takes_commands_out_of_the_batch() {
    let (mut sender, mut inbox) = pair(8, 0);
    send(&mut sender, When::Next, batch(7, &[]));
    inbox.drain();

    let mut due = inbox.next_due(Frame(0), BLOCK).expect("the batch is due");
    let taken: Vec<_> = due.commands_mut().drain(..).collect();
    due.apply(());

    assert_eq!(taken, [7]);
    let receipt = sender.receipts().next().expect("the receipt arrives");
    let (_, returned): Parts = receipt.into();
    assert!(returned.commands.is_empty());
}

#[kithara::test]
fn an_empty_block_applies_nothing() {
    let (mut sender, mut inbox) = pair(8, 0);
    send(&mut sender, When::Next, batch(1, &[]));
    send(&mut sender, When::At(Frame(0)), batch(2, &[]));

    assert!(run_block(&mut inbox, 0, 0).is_empty());
    assert!(outcomes(&mut sender).is_empty());
    assert_eq!(run_block(&mut inbox, 0, BLOCK), [(0, 1), (0, 2)]);
}

#[kithara::test]
fn an_empty_block_answers_nothing() {
    let (mut sender, mut inbox) = pair(8, 0);
    let seq = send(&mut sender, When::At(Frame(10)), batch(1, &[]));

    assert!(run_block(&mut inbox, 64, 0).is_empty());
    assert!(outcomes(&mut sender).is_empty());
    assert!(run_block(&mut inbox, 64, BLOCK).is_empty());
    assert_eq!(
        outcomes(&mut sender),
        [(seq, Outcome::Rejected(Rejection::Late))]
    );
}

#[kithara::test(tokio, browser)]
async fn the_halves_cross_threads() {
    let (mut sender, mut inbox) = pair(8, 0);
    let seq = send(&mut sender, When::Next, batch(1, &[]));

    let (done, executed) = oneshot::channel();
    drop(thread::spawn(move || {
        done.send(run_block(&mut inbox, 0, BLOCK))
            .expect("the test awaits the executor");
    }));
    assert_eq!(executed.await.expect("the executor finishes"), [(0, 1)]);
    assert_eq!(outcomes(&mut sender), [(seq, applied(0))]);
}

#[kithara::test]
fn a_current_basis_applies_the_whole_batch_and_chains() {
    let (mut sender, mut inbox) = pair(8, 2);
    let both = Batch {
        basis: vec![(Slot(0), None), (Slot(1), None)],
        commands: vec![1, 2],
    };
    let first = send(&mut sender, When::Next, both);
    assert_eq!(run_block(&mut inbox, 0, BLOCK), [(0, 1), (0, 2)]);

    let chained = [(Slot(0), Some(first)), (Slot(1), Some(first))];
    send(&mut sender, When::Next, batch(3, &chained));
    assert_eq!(run_block(&mut inbox, 64, BLOCK), [(0, 3)]);
}

#[kithara::test]
fn a_moved_target_rejects_the_whole_batch() {
    let (mut sender, mut inbox) = pair(8, 2);
    let seek = send(&mut sender, When::Next, batch(1, &[(Slot(0), None)]));
    let both = Batch {
        basis: vec![(Slot(0), None), (Slot(1), None)],
        commands: vec![2, 3],
    };
    let stale = send(&mut sender, When::Next, both);

    assert_eq!(run_block(&mut inbox, 0, BLOCK), [(0, 1)]);
    assert_eq!(
        outcomes(&mut sender),
        [
            (seek, applied(0)),
            (stale, Outcome::Rejected(Rejection::Stale)),
        ]
    );

    send(&mut sender, When::Next, batch(4, &[(Slot(1), None)]));
    assert_eq!(run_block(&mut inbox, 64, BLOCK), [(0, 4)]);
}

#[kithara::test]
fn a_disjoint_basis_applies_after_another_target_moves() {
    let (mut sender, mut inbox) = pair(8, 2);
    send(&mut sender, When::Next, batch(1, &[(Slot(0), None)]));
    send(&mut sender, When::Next, batch(2, &[(Slot(1), None)]));

    assert_eq!(run_block(&mut inbox, 0, BLOCK), [(0, 1), (0, 2)]);
}

#[kithara::test]
fn an_empty_basis_applies_and_records_nothing() {
    let (mut sender, mut inbox) = pair(8, 1);
    let seek = send(&mut sender, When::Next, batch(1, &[(Slot(0), None)]));
    send(&mut sender, When::Next, batch(2, &[]));
    send(&mut sender, When::Next, batch(3, &[(Slot(0), Some(seek))]));

    assert_eq!(run_block(&mut inbox, 0, BLOCK), [(0, 1), (0, 2), (0, 3)]);
}

#[kithara::test]
fn a_held_batch_is_judged_when_it_fires() {
    let (mut sender, mut inbox) = pair(8, 1);
    let start = send(&mut sender, When::Next, batch(1, &[(Slot(0), None)]));
    assert_eq!(run_block(&mut inbox, 0, BLOCK), [(0, 1)]);

    let crossfade = send(
        &mut sender,
        When::At(Frame(200)),
        batch(2, &[(Slot(0), Some(start))]),
    );
    assert!(run_block(&mut inbox, 64, BLOCK).is_empty());
    let seek = send(&mut sender, When::Next, batch(3, &[(Slot(0), Some(start))]));
    assert_eq!(run_block(&mut inbox, 128, BLOCK), [(0, 3)]);
    assert!(run_block(&mut inbox, 192, BLOCK).is_empty());

    assert_eq!(
        outcomes(&mut sender),
        [
            (start, applied(0)),
            (seek, applied(128)),
            (crossfade, Outcome::Rejected(Rejection::Stale)),
        ]
    );
}

#[kithara::test]
fn a_repeated_target_applies_only_when_its_entries_agree() {
    let (mut sender, mut inbox) = pair(8, 1);
    let first = send(
        &mut sender,
        When::Next,
        batch(1, &[(Slot(0), None), (Slot(0), None)]),
    );
    send(
        &mut sender,
        When::Next,
        batch(2, &[(Slot(0), Some(first)), (Slot(0), None)]),
    );
    send(
        &mut sender,
        When::Next,
        batch(3, &[(Slot(0), Some(first)), (Slot(0), Some(first))]),
    );

    assert_eq!(run_block(&mut inbox, 0, BLOCK), [(0, 1), (0, 3)]);
}

#[kithara::test]
fn a_dropped_due_batch_comes_back_unanswered() {
    let (mut sender, mut inbox) = pair(1, 0);
    let seq = send(&mut sender, When::Next, batch(1, &[]));
    inbox.drain();
    drop(inbox.next_due(Frame(0), BLOCK));
    let receipt = sender
        .receipts()
        .next()
        .expect("the dropped batch is answered");
    assert_eq!(receipt.seq(), seq);
    let (outcome, returned): Parts = receipt.into();
    assert_eq!(outcome, Outcome::Rejected(Rejection::Unanswered));
    assert_eq!(returned.commands, [1]);
    assert!(sender.send(When::Next, batch(2, &[])).is_ok());
}

#[kithara::test]
fn a_deferred_batch_is_answered_when_its_executor_resumes_it() {
    let (mut sender, inbox) = pair(1, 0);
    let mut inbox = ResumeAt(inbox);
    let seq = send(&mut sender, When::Next, batch(1, &[]));
    inbox.drain();
    let due = inbox.next_due(Frame(64), BLOCK).expect("the batch is due");
    assert_eq!(due.defer(), seq);

    assert!(
        outcomes(&mut sender).is_empty(),
        "a deferred batch waits for its answer"
    );
    assert!(
        matches!(
            sender.send(When::Next, batch(2, &[])),
            Err(SendError::Full(_))
        ),
        "a deferred batch keeps its credit"
    );

    inbox
        .resume(seq, Frame(96))
        .expect("the deferred batch waits in its inbox")
        .apply(());
    assert_eq!(
        outcomes(&mut sender),
        [(seq, applied(96))],
        "a resumed batch applies at the moment it is resumed at"
    );
    assert!(
        inbox.resume(seq, Frame(96)).is_none(),
        "an answered batch is gone"
    );
}

#[kithara::test]
fn a_deferred_batch_a_later_batch_shifted_under_resumes_stale() {
    let (mut sender, inbox) = pair(8, 1);
    let mut inbox = ResumeAt(inbox);
    let deferred = send(&mut sender, When::Next, batch(1, &[(Slot(0), None)]));
    let shift = send(&mut sender, When::Next, batch(2, &[(Slot(0), None)]));
    inbox.drain();
    inbox
        .next_due(Frame(0), BLOCK)
        .expect("the first batch is due")
        .defer();
    assert_eq!(run_block(&mut inbox, 0, BLOCK), [(0, 2)]);

    assert!(inbox.resume(deferred, Frame(32)).is_none());
    assert_eq!(
        outcomes(&mut sender),
        [
            (shift, applied(0)),
            (deferred, Outcome::Rejected(Rejection::Stale))
        ]
    );
}

#[kithara::test]
fn a_deferred_batch_leaves_the_block_to_the_batches_after_it() {
    let (mut sender, inbox) = pair(8, 0);
    let mut inbox = ResumeAt(inbox);
    let deferred = send(&mut sender, When::Next, batch(1, &[]));
    let next = send(&mut sender, When::Next, batch(2, &[]));
    inbox.drain();
    inbox
        .next_due(Frame(0), BLOCK)
        .expect("the first batch is due")
        .defer();

    assert_eq!(run_block(&mut inbox, 0, BLOCK), [(0, 2)]);
    assert_eq!(outcomes(&mut sender), [(next, applied(0))]);
    drop(inbox.resume(deferred, Frame(0)));
    assert_eq!(
        outcomes(&mut sender),
        [(deferred, Outcome::Rejected(Rejection::Unanswered))]
    );
}

#[kithara::test]
fn a_send_wakes_an_executor_waiting_on_its_inbox() {
    let (mut sender, mut inbox) = pair(8, 0);
    let (waker, wakes) = waker();
    let mut cx = Context::from_waker(&waker);
    assert_eq!(inbox.poll_drain(&mut cx), Poll::Pending, "nothing waits");

    send(&mut sender, When::Next, batch(1, &[]));

    assert_eq!(wakes.count(), 1, "one send, one wake");
    assert_eq!(inbox.poll_drain(&mut cx), Poll::Ready(()));
    assert_eq!(run_block(&mut inbox, 0, BLOCK), [(0, 1)]);
}

#[kithara::test]
fn a_dropped_sender_closes_the_inbox_and_wakes_its_executor() {
    let (sender, mut inbox) = pair(8, 0);
    let (waker, wakes) = waker();
    let mut cx = Context::from_waker(&waker);
    assert_eq!(inbox.poll_drain(&mut cx), Poll::Pending, "nothing waits");
    assert!(!inbox.is_closed(), "the sender still sends");

    drop(sender);

    assert_eq!(wakes.count(), 1, "the sender's drop wakes its executor");
    assert!(inbox.is_closed());
    assert_eq!(
        inbox.poll_drain(&mut cx),
        Poll::Ready(()),
        "a closed inbox leaves nothing to wait for"
    );
}

#[kithara::test]
fn a_dropped_inbox_answers_every_batch_it_holds() {
    let (mut sender, mut inbox) = pair(8, 0);
    let parked = send(&mut sender, When::Next, batch(1, &[]));
    let scheduled = send(&mut sender, When::At(Frame(1_000)), batch(2, &[]));
    inbox.drain();
    inbox
        .next_due(Frame(0), BLOCK)
        .expect("the first batch is due")
        .defer();
    let pending = send(&mut sender, When::Next, batch(3, &[]));

    drop(inbox);

    let mut answered: Vec<_> = sender
        .receipts()
        .map(|receipt| {
            let seq = receipt.seq();
            let (outcome, returned): Parts = receipt.into();
            (seq, outcome, returned.commands)
        })
        .collect();
    answered.sort_by_key(|&(seq, ..)| seq);
    let unanswered = || Outcome::Rejected(Rejection::Unanswered);
    assert_eq!(
        answered,
        [
            (parked, unanswered(), vec![1]),
            (scheduled, unanswered(), vec![2]),
            (pending, unanswered(), vec![3]),
        ],
        "parked, scheduled and undrained batches each come back whole"
    );
}

/// An inbox that dropped answers nothing more, so a later send comes back
/// whole instead of waiting on a ring nobody drains.
#[kithara::test]
fn a_batch_sent_after_the_inbox_dropped_comes_back_closed() {
    let (mut sender, inbox) = pair(8, 0);

    drop(inbox);

    let Err(SendError::Closed(returned)) = sender.send(When::Next, batch(1, &[])) else {
        panic!("a send to a dropped inbox was taken");
    };
    assert_eq!(returned.commands, [1], "the batch comes back whole");
    assert_eq!(sender.receipts().count(), 0, "nothing was sent to answer");
}

/// An owner holding its sender is woken by each receipt, so it reads the
/// answer without waiting for a tick of its own.
#[kithara::test]
fn a_held_sender_is_woken_by_each_receipt() {
    let (mut sender, mut inbox) = pair(8, 0);
    let (waker, wakes) = waker();
    sender.hold(waker);
    send(&mut sender, When::Next, batch(1, &[]));
    send(&mut sender, When::Next, batch(2, &[]));
    inbox.drain();

    inbox
        .next_due(Frame(0), BLOCK)
        .expect("the first batch is due")
        .apply(());
    assert_eq!(wakes.count(), 1, "the receipt wakes the sender's owner");
    assert_eq!(sender.receipts().count(), 1);
    inbox
        .next_due(Frame(0), BLOCK)
        .expect("the second batch is due")
        .apply(());

    assert_eq!(
        wakes.count(),
        2,
        "the owner reading its receipts stays held for the next one"
    );
}

/// A released sender's owner reads its receipts on its own time.
#[kithara::test]
fn a_released_sender_is_not_woken_by_a_receipt() {
    let (mut sender, mut inbox) = pair(8, 0);
    let (waker, wakes) = waker();
    sender.hold(waker);
    sender.release();
    send(&mut sender, When::Next, batch(1, &[]));
    inbox.drain();

    inbox
        .next_due(Frame(0), BLOCK)
        .expect("the batch is due")
        .apply(());

    assert_eq!(wakes.count(), 0);
    assert_eq!(sender.receipts().count(), 1);
}

/// A batch built on the sender's basis for a target applies after the batch
/// before it shifted that target; once a batch on it comes back rejected, the
/// basis falls back to the last one that applied.
#[kithara::test]
fn the_sender_bases_each_batch_on_the_last_one_that_shifts_its_target() {
    let (sender, mut inbox) = pair(4, 2);
    let mut sender = BasisAt(sender);
    assert_eq!(sender.basis(Slot(0)), None);

    let first = send(&mut sender, When::Next, batch(1, &[(Slot(0), None)]));
    let basis = sender.basis(Slot(0));
    let second = send(&mut sender, When::Next, batch(2, &[(Slot(0), basis)]));
    assert_eq!(sender.basis(Slot(0)), Some(second));
    assert_eq!(sender.basis(Slot(1)), None, "another target is untouched");
    assert_eq!(run_block(&mut inbox, 0, BLOCK), [(0, 1), (0, 2)]);
    drop(outcomes(&mut sender));

    let basis = sender.basis(Slot(0));
    let late = send(
        &mut sender,
        When::At(Frame(0)),
        batch(3, &[(Slot(0), basis)]),
    );
    assert_eq!(sender.basis(Slot(0)), Some(late));
    assert!(run_block(&mut inbox, BLOCK as u64, BLOCK).is_empty());
    assert_eq!(
        outcomes(&mut sender),
        [(late, Outcome::Rejected(Rejection::Late))]
    );

    assert_eq!(sender.basis(Slot(0)), Some(second));
    assert_ne!(first, second);
}

#[kithara::test]
fn a_next_basis_excludes_pending_future_shifts() {
    let (mut sender, _inbox) = pair(4, 1);
    send(
        &mut sender,
        When::At(Frame(200)),
        batch(1, &[(Slot(0), None)]),
    );
    assert_eq!(sender.basis(Slot(0), When::Next), None);
}

#[kithara::test]
fn basis_projection_skips_stale_pending_batches_in_moment_order() {
    let (mut sender, mut inbox) = pair(4, 1);
    let future = send(
        &mut sender,
        When::At(Frame(200)),
        batch(1, &[(Slot(0), None)]),
    );
    let next = send(&mut sender, When::Next, batch(2, &[(Slot(0), None)]));
    let dependent = send(
        &mut sender,
        When::At(Frame(250)),
        batch(3, &[(Slot(0), Some(future))]),
    );
    assert_eq!(sender.basis(Slot(0), When::At(Frame(199))), Some(next));
    assert_eq!(sender.basis(Slot(0), When::At(Frame(200))), Some(next));
    assert_eq!(sender.basis(Slot(0), When::At(Frame(250))), Some(next));
    let last = send(
        &mut sender,
        When::At(Frame(300)),
        batch(4, &[(Slot(0), Some(next))]),
    );
    assert_eq!(sender.basis(Slot(0), When::At(Frame(300))), Some(last));
    assert_eq!(sender.available(), 0);
    assert!(matches!(
        sender.send(When::Next, batch(5, &[(Slot(0), Some(next))])),
        Err(SendError::Full(_))
    ));
    assert_eq!(sender.basis(Slot(0), When::Next), Some(next));
    assert_eq!(run_block(&mut inbox, 64, BLOCK), [(0, 2)]);
    assert_eq!(
        outcomes(&mut sender),
        [
            (next, applied(64)),
            (future, Outcome::Rejected(Rejection::Stale)),
            (dependent, Outcome::Rejected(Rejection::Stale)),
        ]
    );
    assert_eq!(run_block(&mut inbox, 250, BLOCK), [(50, 4)]);
    assert_eq!(outcomes(&mut sender), [(last, applied(300))]);
    assert_eq!(sender.available(), 4);
}

#[kithara::test]
fn deferred_resume_rejects_a_future_basis_that_never_became_current() {
    let (mut sender, mut inbox) = pair(2, 1);
    let future = send(
        &mut sender,
        When::At(Frame(200)),
        batch(1, &[(Slot(0), None)]),
    );
    let deferred = send(
        &mut sender,
        When::Deferred,
        batch(2, &[(Slot(0), Some(future))]),
    );
    inbox.drain();
    assert!(outcomes(&mut sender).is_empty());
    assert_eq!(
        inbox.next_deferred().expect("arrival").park(),
        Some(deferred)
    );
    assert!(inbox.resume(deferred, Frame(64), Frame(70)).is_none());
    assert!(!inbox.is_parked(deferred));
    assert_eq!(
        outcomes(&mut sender),
        [(deferred, Outcome::Rejected(Rejection::Stale))]
    );
    assert_eq!(run_block(&mut inbox, 200, BLOCK), [(0, 1)]);
    assert_eq!(outcomes(&mut sender), [(future, applied(200))]);
}

#[kithara::test]
fn apply_answers_outdated_future_batches_in_the_same_block() {
    let (mut sender, mut inbox) = pair(8, 2);
    let future = send(
        &mut sender,
        When::At(Frame(200)),
        batch(1, &[(Slot(0), None)]),
    );
    let shift = send(&mut sender, When::Next, batch(2, &[(Slot(0), None)]));
    let equal = send(
        &mut sender,
        When::At(Frame(250)),
        batch(3, &[(Slot(0), Some(shift))]),
    );
    let above = send(
        &mut sender,
        When::At(Frame(300)),
        batch(4, &[(Slot(0), Some(equal))]),
    );
    let disjoint = send(
        &mut sender,
        When::At(Frame(400)),
        batch(5, &[(Slot(1), None)]),
    );
    inbox.drain();
    assert!(outcomes(&mut sender).is_empty(), "drain never judges");
    inbox
        .next_due(Frame(64), BLOCK)
        .expect("Next is due")
        .apply(());
    assert_eq!(
        outcomes(&mut sender),
        [
            (shift, applied(64)),
            (future, Outcome::Rejected(Rejection::Stale)),
        ]
    );
    assert_eq!(inbox.frames_until_due(Frame(64)), Some(186));
    assert_eq!(run_block(&mut inbox, 250, BLOCK), [(0, 3), (50, 4)]);
    assert_eq!(run_block(&mut inbox, 400, BLOCK), [(0, 5)]);
    assert_eq!(
        outcomes(&mut sender),
        [
            (equal, applied(250)),
            (above, applied(300)),
            (disjoint, applied(400))
        ]
    );
}

#[kithara::test]
fn deferred_is_after_every_at_and_never_judged_by_next_due() {
    let (mut sender, mut inbox) = pair(4, 1);
    assert!(When::At(Frame(u64::MAX)) < When::Deferred);
    let deferred = send(&mut sender, When::Deferred, batch(1, &[(Slot(0), None)]));
    assert_eq!(sender.basis(Slot(0), When::Next), None);
    assert_eq!(sender.basis(Slot(0), When::At(Frame(u64::MAX))), None);
    assert_eq!(sender.basis(Slot(0), When::Deferred), Some(deferred));
    inbox.drain();
    assert_eq!(inbox.frames_until_due(Frame(0)), None);
    assert!(inbox.next_due(Frame(0), BLOCK).is_none());
    assert!(outcomes(&mut sender).is_empty());
    let arrived = inbox
        .next_deferred()
        .expect("a deferred arrival is exposed");
    assert_eq!(arrived.seq(), deferred);
    assert_eq!(arrived.basis(), [(Slot(0), None)]);
    assert_eq!(arrived.commands(), [1]);
    drop(arrived);
    assert_eq!(
        outcomes(&mut sender),
        [(deferred, Outcome::Rejected(Rejection::Unanswered))]
    );
    assert!(inbox.next_deferred().is_none());
}

#[kithara::test]
fn deferred_resume_uses_its_firing_block_and_judges_its_basis() {
    let (mut sender, mut inbox) = pair(8, 1);
    let pending = send(&mut sender, When::Deferred, batch(1, &[(Slot(0), None)]));
    inbox.drain();
    assert_eq!(
        inbox.next_deferred().expect("arrival").park(),
        Some(pending)
    );
    assert!(inbox.is_parked(pending));
    let due = inbox
        .resume(pending, Frame(128), Frame(150))
        .expect("parked");
    assert_eq!(due.offset(), 22);
    due.apply(());
    assert!(!inbox.is_parked(pending));
    assert!(inbox.resume(pending, Frame(128), Frame(150)).is_none());
    let stale = send(&mut sender, When::Deferred, batch(2, &[(Slot(0), None)]));
    inbox.drain();
    assert!(inbox.next_deferred().expect("arrival").park().is_none());
    let refused = send(&mut sender, When::Deferred, batch(3, &[]));
    inbox.drain();
    inbox
        .next_deferred()
        .expect("arrival")
        .refuse("bad deferral");
    assert_eq!(
        outcomes(&mut sender),
        [
            (pending, applied(150)),
            (stale, Outcome::Rejected(Rejection::Stale)),
            (
                refused,
                Outcome::Rejected(Rejection::Refused("bad deferral"))
            ),
        ]
    );
}

#[kithara::test]
fn commit_records_basis_and_eager_stales_before_answer() {
    let (mut sender, mut inbox) = pair(6, 2);
    let parked = send(&mut sender, When::Next, batch(1, &[(Slot(0), None)]));
    let committed = send(
        &mut sender,
        When::At(Frame(80)),
        batch(2, &[(Slot(0), None)]),
    );
    let scheduled = send(
        &mut sender,
        When::At(Frame(96)),
        batch(3, &[(Slot(0), None)]),
    );
    let deferred = send(&mut sender, When::Deferred, batch(4, &[(Slot(0), None)]));
    let current = send(
        &mut sender,
        When::At(Frame(100)),
        batch(5, &[(Slot(0), Some(committed))]),
    );
    let independent = send(
        &mut sender,
        When::At(Frame(100)),
        batch(6, &[(Slot(1), None)]),
    );
    inbox.drain();
    inbox
        .next_due(Frame(64), BLOCK)
        .expect("parked due")
        .defer();
    assert_eq!(
        inbox.next_due(Frame(64), BLOCK).expect("commit").commit(),
        committed
    );
    assert_eq!(sender.available(), 0, "commit does not return its credit");
    let stale = outcomes(&mut sender);
    for seq in [parked, scheduled, deferred] {
        assert!(stale.contains(&(seq, Outcome::Rejected(Rejection::Stale))));
    }
    assert_eq!(stale.len(), 3);
    assert_eq!(sender.available(), 3);
    assert_eq!(run_block(&mut inbox, 96, BLOCK), [(4, 5), (4, 6)]);
    assert_eq!(
        outcomes(&mut sender),
        [(current, applied(100)), (independent, applied(100))]
    );
    assert_eq!(inbox.committed_mut(committed), Some([2].as_mut_slice()));
    assert_eq!(sender.available(), 5, "the committed credit is still held");
}

#[kithara::test]
fn completion_keeps_original_moment_after_later_ledger_shift() {
    let (mut sender, mut inbox) = pair(2, 1);
    let committed = send(
        &mut sender,
        When::At(Frame(80)),
        batch(1, &[(Slot(0), None)]),
    );
    let later = send(
        &mut sender,
        When::At(Frame(100)),
        batch(2, &[(Slot(0), Some(committed))]),
    );
    inbox.drain();
    inbox.next_due(Frame(64), BLOCK).expect("commit").commit();
    inbox
        .next_due(Frame(64), BLOCK)
        .expect("later shift")
        .apply(());
    assert_eq!(outcomes(&mut sender), [(later, applied(100))]);
    assert!(!inbox.is_parked(committed));
    assert!(inbox.resume(committed, Frame(256), Frame(270)).is_none());
    assert!(inbox.next_due(Frame(256), BLOCK).is_none());
    inbox.committed_mut(committed).expect("held commands")[0] = 99;
    assert!(inbox.complete(committed, ()));
    let receipt = sender.receipts().next().expect("completed");
    assert_eq!(receipt.seq(), committed);
    assert_eq!(receipt.outcome(), &applied(80));
    assert_eq!(receipt.batch().commands, [99]);
    assert_eq!(receipt.batch().basis, [(Slot(0), None)]);
    assert!(!inbox.complete(committed, ()));
    assert!(inbox.committed_mut(committed).is_none());
    assert_eq!(sender.available(), 2);
}

#[kithara::test]
fn unfinished_commits_remain_owned_until_inbox_drop() {
    let (mut sender, mut inbox) = pair(2, 1);
    let committed = send(&mut sender, When::Next, batch(1, &[(Slot(0), None)]));
    inbox.drain();
    inbox.next_due(Frame(64), BLOCK).expect("commit").commit();
    inbox.committed_mut(committed).expect("held commands")[0] = 99;
    inbox.refuse_timed("axis changed");
    assert!(sender.receipts().next().is_none());
    assert!(inbox.next_due(Frame(1024), BLOCK).is_none());
    assert_eq!(sender.available(), 1);
    drop(inbox);
    let receipt = sender.receipts().next().expect("leftover");
    assert_eq!(receipt.seq(), committed);
    assert_eq!(receipt.outcome(), &Outcome::Rejected(Rejection::Unanswered));
    assert_eq!(receipt.batch().commands, [99]);
    assert_eq!(sender.available(), 2);
}

#[kithara::test]
fn delayed_applied_receipt_does_not_roll_back_sender_basis() {
    let (mut sender, mut inbox) = pair(2, 1);
    let committed = send(&mut sender, When::Next, batch(1, &[(Slot(0), None)]));
    let later = send(
        &mut sender,
        When::At(Frame(100)),
        batch(2, &[(Slot(0), Some(committed))]),
    );
    inbox.drain();
    inbox.next_due(Frame(64), BLOCK).expect("commit").commit();
    inbox
        .next_due(Frame(64), BLOCK)
        .expect("later shift")
        .apply(());
    assert_eq!(outcomes(&mut sender), [(later, applied(100))]);
    assert_eq!(sender.basis(Slot(0), When::Next), Some(later));
    assert!(inbox.complete(committed, ()));
    assert_eq!(outcomes(&mut sender), [(committed, applied(64))]);
    assert_eq!(sender.basis(Slot(0), When::Next), Some(later));
    let following = send(&mut sender, When::Next, batch(3, &[(Slot(0), Some(later))]));
    assert_eq!(run_block(&mut inbox, 256, BLOCK), [(0, 3)]);
    assert_eq!(outcomes(&mut sender), [(following, applied(256))]);
}

struct BasisAt(Sender<Test>);

impl BasisAt {
    fn basis(&self, target: Slot) -> Option<Seq> {
        self.0.basis(target, When::At(Frame(u64::MAX)))
    }
}

impl Deref for BasisAt {
    type Target = Sender<Test>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for BasisAt {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

struct ResumeAt(Inbox<Test>);

impl ResumeAt {
    fn resume(&mut self, seq: Seq, at: Frame) -> Option<super::Due<'_, Test>> {
        self.0.resume(seq, at, at)
    }
}

impl Deref for ResumeAt {
    type Target = Inbox<Test>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for ResumeAt {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}
