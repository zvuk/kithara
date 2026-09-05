# kithara-bufpool — Context

Contracts and invariants for kithara-bufpool; the README is the overview.

## Ownership

`PoolRegion<S>` owns one immutable schema and one shared `RegionBudget`.
Cloning the facade clones that owner handle; it does not create another pool or
counter. Composition roots declare concrete schemas with `pool_schema!` and
register every slot through the generated typestate builder. Missing,
duplicate, unknown, and unsupported registrations fail to compile.

Lower crates accept `PoolRegion<S>` with the narrow `S: HasPool<K>` bounds they
need. `PoolAlias<Tag, K>` names a distinct physical slot with an existing key's
element and guard policy; aliases do not share a free list or per-pool counter.
`VecKey<T, N>` and `StringKey<N>` are the sealed extension points for
crate-owned vector and UTF-8 aliases; downstream crates cannot implement raw
key plumbing or detach a slot from its region.

The public macro must expose construction plumbing to downstream expansion.
Every slot therefore carries its region provenance, checked on every dispatch.
Raw key operations additionally require an unforgeable crate-owned capability.
Safe downstream code cannot detach a core or attach a slot to another region's
reported budget.

With the `test-utils` feature, `testing::TestPools` provides the same byte and
sample capabilities as the application composition roots. Each test harness
builds one isolated region from that schema and passes the facade through the
subsystems it composes; tests do not declare narrower per-crate schemas or share
one process-global region.

## Allocation flow

The built-in byte key uses 32 shards and the sample key uses 8. Generic vector
and string keys state their shard count in the key type. Each shard is a bounded
`crossbeam_queue::ArrayQueue`; acquisition and return take no lock.

1. `get` probes the calling thread's shard, up to four neighbouring shards, and
   the optional cold-start queue. A complete miss returns a fresh empty guard.
1. Checked growth reserves capacity against both hard budgets before allocation,
   reconciles allocator capacity, and rolls both counters back on failure. If a
   new reservation cannot fit, growth makes one bounded pass over every shard
   and then the cold-start queue for an already charged suitable allocation. A
   remaining pool-cap rejection evicts idle allocations from that slot; an
   overall-cap rejection evicts idle allocations across registered slots before
   one final admission attempt.
1. Guard drop clears its vector or string, applies the configured trim or strict
   retained-capacity ceiling, and returns it to the home shard. A rejected return
   drops the allocation and releases its complete charge.

`max_buffers` is divided across shards and each shard is capped at 1024 retained
values. It must provide at least one slot per shard. `initial_buffers` and
`initial_capacity` allocate payloads during construction; any failure drops all
earlier slots and no region is published.

The region freezes a weak inventory of slot reclaimers only after the complete
schema builds. The inventory cannot keep a slot alive, and pressure reclamation
never reaches a checked-out guard.

## Budget contract

`OverallBudget` is the hard cap shared by every slot in a region. A slot's
`max_share` is an additional hard ceiling, not a reservation or partition: two
slots may both use `Percent::FULL` and compete for the same peak capacity.

Accounting measures `Vec` capacity multiplied by element size and `String`
capacity in bytes. It deliberately does not claim to measure RSS, allocator
metadata, or the temporary old-plus-new allocation during transactional growth.
Every retained or checked-out pooled allocation remains charged until trimming
or dropping returns bytes.

Growth chooses one amortized target, clamped to the capacity currently available
under both counters. This keeps incremental writes linear without an unchecked
exact-fit fallback. A budget rejection may discard bounded idle retention and
repeat admission once for each hard counter it encounters; a concurrent
reservation may still make that attempt fail immediately. On any error, the
original buffer and both counters stay unchanged.

## Buffer boundary

`ByteBuffer`, `SampleBuffer`, and `PooledVec` dereference to slices, never raw
`Vec`s. Built-in capacity grows through `ensure_len` and
`try_extend_from_slice`; generic vectors use `try_push` and `try_extend`.
`PooledString` keeps UTF-8 validity in `String` and grows through
`try_push_str`. There is no public raw growth, extraction, attach, recycle,
collect, or pre-warm contract.

`ByteBuffer::renew` returns its held allocation to the slot and continues with
another empty guard, so a long-lived owner can release active capacity for
region-wide reclaim. `ByteBuffer::normalize` instead clears and normalizes the
held allocation in place. It applies the same retained-capacity policy as guard
return and lets long-lived scratch reuse one bounded allocation.

`BufferRing<B>` is an owning FIFO view over one already-sized buffer. It never
grows or shifts retained values; failed pushes and pops leave both sides
unchanged, and `into_inner` returns the original pool guard to its owner.

`stats()` reports the shared region budget. `pool_stats::<K>()` reports reuse for
one registered generic slot; built-in hot-path keys compile out counter updates.
