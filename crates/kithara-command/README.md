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
use kithara_command::{Batch, ChannelConfig, Clock, Protocol, Target, When, channel};

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

impl Clock for Frame {
    fn frames_since(self, start: Self) -> Option<u64> {
        self.0.checked_sub(start.0)
    }
}

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

## Key Types

<table>

<tr><th>Type</th><th>Role</th></tr>

<tr><td><code>Protocol</code></td><td>Names one executor's command, target, clock, and answer types</td></tr>

<tr><td><code>Batch</code></td><td>Commands applied together, with the basis they were computed from</td></tr>

<tr><td><code>Sender</code> / <code>Inbox</code></td><td>Halves of a channel: numbering and sending, and scheduling, judging, and answering</td></tr>

<tr><td><code>Due</code></td><td>A batch due inside the current block, which the executor applies or refuses</td></tr>

<tr><td><code>Receipt</code></td><td>What became of a batch, carrying the batch back to the sender</td></tr>

<tr><td><code>Live</code></td><td>A live configuration as its executor confirmed it, with copies of the changes in flight</td></tr>

</table>

## Integration

The real-time Host, the lane dispatcher, and each lane own one `Inbox` each
and speak their own `Protocol`; the owner that computes commands holds the
matching `Sender`. The crate carries no audio domain: frames, targets, and
commands are the executor's types.

### Live configuration

`Live<C, P>` keeps a `LiveConfig` on the sender's side of an executor. `send`
checks a field change and hands it over as one batch with an empty basis and
the single command `wrap` makes of it; the configuration changes only when
`settle` meets the batch's receipt as applied, so getters read what the
executor confirmed. `apply` changes a field at once when no executor receives
it, and `abandon` folds the copies still in flight in `(When, Seq)` order when
the queue goes away unanswered.

See [crate contracts](https://github.com/zvuk/kithara/wiki/kithara-command) for detailed contracts, invariants, and internals.
