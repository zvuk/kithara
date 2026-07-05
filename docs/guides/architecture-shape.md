# Architecture Shape

Use this for owner graphs, shared state, channels, god objects, and callback
flows.

## Owners

- Every mutable state has one owner.
- Shared handles observe or send commands; they do not become parallel owners.
- `Arc<...>` is not ownership design. It is acceptable only after naming the
  owner and why borrowing, ownership transfer, snapshots, or commands do not fit.
- `Arc<Atomic*>` is a red flag unless it is a tiny cross-thread signal owned by
  one component, with documented semantics and memory ordering.
- Root types coordinate smaller owners; they do not accumulate every map, flag,
  queue, counter, and protocol rule.

File splits are not architecture fixes by themselves. Ownership must move too.

## Flow

Components may talk only to adjacent owners.

```text
A -> B -> C
A <- B <- C
```

Avoid non-adjacent access:

```text
A -> C
C -> A
```

Avoid call spirals where several owners know and mutate each other through
callbacks:

```text
A.foo() -> B.bar() -> C.baz() -> B.daz() -> A.haz()
```

If this appears, look for a missing coordinator, wrong state owner, or state that
should be represented as a typed state machine.

## Channels

Channels do not bypass ownership rules. A good channel creates one state owner:

```text
A --command--> B owner --command--> C
A <--event/snapshot-- B <--result-- C
```

A bad channel hides an async call spiral:

```text
A sends to B
B sends to C
C sends to B
B sends to A
plus Arc<Atomic*> side flags
```

Use channels when:

- there is one owner of mutable state;
- messages are typed enums;
- queue bounds and backpressure are explicit;
- lifecycle, cancel, drain, and shutdown are owned;
- responses are typed results, snapshots, events, or `oneshot`s.

Avoid channels when they replace a simple function call, create unbounded queues,
let multiple consumers mutate one domain state, or require side-channel atomics to
recover meaning.
