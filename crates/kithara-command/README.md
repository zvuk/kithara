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

### More

- **Scoped channels.** `scoped_channel::<Root, Scope>(ScopedConfig)` adds
  reusable scope slots beside the root level; a pass of sends becomes visible
  at once on `publish()`, and `close(id)` retires a scope, returning leftover
  batches as Unanswered.
- **Deferred batches.** `When::Deferred` waits until the executor names its
  moment: `next_deferred()`, `park()`, then `resume(seq, start, at)`.
- **Two-phase apply.** `Due::commit()` records the batch now;
  `complete(seq, data)` answers it later.
- **Basis.** `Port::basis(target, when)` gives the state a new batch should be
  computed from; a batch computed from an outdated basis comes back Stale.

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

The executor owns an `Inbox` (or a `LevelInbox` of a `ScopedInbox`) and the
owner that computes commands holds the `Port`. `Live` keeps a configuration on
the owner side that changes only when the executor confirms it, so getters read
what is actually running. The crate has no audio types: frames, targets, and
commands belong to the executor's `Protocol`.

See [crate contracts](https://github.com/zvuk/kithara/wiki/kithara-command) for detailed contracts, invariants, and internals.
