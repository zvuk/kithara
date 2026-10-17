<div align="center">

<img src="https://raw.githubusercontent.com/zvuk/kithara/main/logo.svg" alt="kithara" width="300">

</div>

<div align="center">

[![crates.io](https://img.shields.io/crates/v/kithara-command.svg)](https://crates.io/crates/kithara-command)
[![docs.rs](https://docs.rs/kithara-command/badge.svg)](https://docs.rs/kithara-command)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](https://github.com/zvuk/kithara/blob/main/LICENSE-MIT)

</div>

# kithara-command

Timestamped command batches for real-time executors. A sender numbers each
batch and hands it over with the frame it applies at; the executor takes the
batches due inside each block in time order, judges each against the basis it
was computed from, and returns every batch whole in a receipt, so the
executor's thread never allocates or drops.

## Usage

```rust
use kithara_command::{Batch, ChannelConfig, Protocol, Target, When, channel};

#[derive(Debug)]
struct Deck;

#[derive(Clone, Copy, Debug)]
struct Slot(usize);

impl Target for Slot {
    fn index(self) -> usize {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Frame(u64);

#[derive(Debug)]
enum Command {
    Seek(u64),
}

impl Protocol for Deck {
    type Applied = ();
    type Clock = Frame;
    type Command = Command;
    type Refusal = ();
    type Target = Slot;

    fn frames_since(at: Frame, start: Frame) -> Option<u64> {
        at.0.checked_sub(start.0)
    }
}

let (mut sender, mut inbox) = channel::<Deck>(ChannelConfig::builder().targets(1).build());
let seek = Batch {
    basis: vec![(Slot(0), None)],
    commands: vec![Command::Seek(48_000)],
};
let Ok(seq) = sender.send(When::At(Frame(100)), seek) else {
    unreachable!("an empty channel has room");
};

inbox.drain();
while let Some(due) = inbox.next_due(Frame(64), 64) {
    assert_eq!(due.offset(), 36);
    due.apply(());
}
assert_eq!(sender.receipts().next().map(|receipt| receipt.seq()), Some(seq));
```

### Scoped publication

`scoped_channel::<Root, Scope>(ScopedConfig)` reserves independent credits and
target ledgers for a root and a fixed number of reusable scope slots. The owner
opens a slot with `open(targets)` and borrows its `ScopeSender` with `scope(id)`.
Both ports implement `Port`; sending stages a batch, and `publish()` makes the
whole pass visible with one ring-index store. Draining before publication sees
none of that pass. Execution order is `(When, Seq)` within each level; the send
counter is shared, but levels are walked independently.

`close(id)` uses reserved room even when the scope has no normal credits left.
After a drain exposes Close, the executor finishes its walk and calls
`LevelInbox::retire()` when it is ready. With no running executor, the owner can
call `retire_closing()`. Retirement returns leftovers whole as Unanswered and
sends `ScopedReceipt::Closed(id)` last. Retirement advances the generation;
reading Closed frees the owner slot. Reopening uses the new generation, so the
old ID no longer borrows a level.

`ScopedConfig::builder()` takes `root(ChannelConfig)`, `scope(ChannelConfig)`
and `scopes(NonZeroU16)`. The scope config's target count is the maximum admitted
by `open`; all executor storage is reserved when the channel is built.

### Basis and deferred batches

`Port::basis(target, when)` projects applied receipts and pending batches up to
`when`, skipping batches whose basis would be Stale. A future At or Deferred
does not shift the basis of a Next. An accepted race remains: a Next can land
after an already-pending At on the same target; it then receives Stale, and the
owner plans from the At receipt.

`When::Deferred` sorts after every At and is never returned by `next_due`.
`next_deferred()` exposes the unjudged arrival; `park()` keeps it until an
executor event names its moment. `resume(seq, start, at)` judges it and computes
the offset from the firing block's start. Applying a batch eagerly returns all
outdated scheduled, arrived and parked batches of that level as Stale.

`Due::commit()` records the ledger and eagerly invalidates outdated batches at
the due moment, but leaves its batch and credit in the inbox. The executor can
fill result commands through `committed_mut(seq)` and later call
`complete(seq, data)` on `Inbox` or `LevelInbox`. Completion uses the original
moment and never judges the basis again. An unfinished committed batch returns
whole through the existing retirement or inbox-drop path as Unanswered.

## Key Types

<table>

<tr><th>Type</th><th>Role</th></tr>

<tr><td><code>Protocol</code></td><td>Names one executor's command, target, clock, and answer types, and counts frames on its clock</td></tr>

<tr><td><code>Batch</code></td><td>Commands applied together, with the basis they were computed from</td></tr>

<tr><td><code>Sender</code> / <code>Inbox</code></td><td>Halves of a channel: numbering and sending, and scheduling, judging, and answering</td></tr>

<tr><td><code>Due</code></td><td>A batch due inside the current block, which the executor applies or refuses</td></tr>

<tr><td><code>Receipt</code></td><td>What became of a batch, carrying the batch back to the sender</td></tr>

<tr><td><code>Live</code></td><td>A live configuration as its executor confirmed it, with copies of the changes in flight</td></tr>

</table>

## Integration

An executor owns a plain `Inbox`, or borrows a `LevelInbox` from a shared
`ScopedInbox`, and speaks its own `Protocol`. The owner that computes commands
holds the matching `Port`. Both forms use the same scheduling and judging
mechanism. The crate carries no audio domain: frames, targets, and commands
are the executor's types.

### Live configuration

`Live<C, P>` keeps a `LiveConfig` on the owner's side of an executor. `send`
checks a field change and hands it over as one batch with an empty basis and
the single command `wrap` makes of it, through any `Port<P>`. `track` records a
checked change carried by a batch the owner built itself. The configuration
changes only when
`settle` meets the batch's receipt as applied, so getters read what the
executor confirmed. Settle receipts in the execution order returned by
`Sender::receipts()`; reordering them can leave the configuration different
from the executor's when changes share a field. `apply` changes a field at
once when no executor receives it, and `abandon` folds the copies still in
flight in `(When, Seq)` order when the queue goes away unanswered.

See [crate contracts](https://github.com/zvuk/kithara/wiki/kithara-command) for detailed contracts, invariants, and internals.
