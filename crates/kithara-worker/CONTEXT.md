# Worker Runtime Contract

## Ownership

`Worker` owns shared runtime resources and the root of its cancellation
subtree. Each `Dispatcher` owns one scheduler thread. Each admitted task owns a
reservation, a child cancellation token, and a single mutable numeric priority.
`PendingTask::start` constructs transferable tasks on the caller, while
`PendingTask::start_local` constructs thread-bound tasks on the dispatcher and
keeps them there for their complete lifecycle.

The crate contains no playback, analysis, asset, or media policy. Domain crates
provide `Task` implementations and consume observer events.

## Cancellation

The vertical lineage is always `Worker -> Dispatcher -> Task -> Compute job`.
Additional domain cancellation sources are OR-composed with the derived task
token. They cancel only that task token and wake its dispatcher; they never
replace or widen the vertical lineage.

A compute job observes only its child token. Task and domain cancellation have
already been folded into the task token, so repeating ancestor sources in the
compute group would expose cancellation before propagation reaches the child.

## Scheduling and compute

The scheduler thread owns task order and lifecycle callbacks. A slot also owns
idempotent cancellation cleanup so a queued registration discarded during
scheduler teardown still receives `on_cancel` and `recycle` exactly once.
Higher numeric priority runs first, with stable task ID order as the tie-break.
Immediate wake may unpark the thread; deferred wake is a coalesced atomic signal
suitable for real-time callers and publishes writes made before it.

Rayon compute is explicitly disabled, shared from an external owner, or lazily
owned. A lazy pool is built only after the first job passes both admission
limits. Compute submission has per-task and worker-wide in-flight limits and no
hidden queue. Every rejection returns its caller-owned payload unchanged. A
saturated task may retry on a later scheduler tick after completion wakes the
dispatcher. WebAssembly exposes only the disabled mode.

The command channel is unbounded as a primitive, but task capacity bounds its
producers: one reservation can enqueue one registration and its non-cloneable
handle can enqueue one removal. Admission and shutdown serialize through one
lifecycle lock, so shutdown closes admission before enqueueing its terminal
command. Handle drop enqueues removal before releasing its reservation, so an
immediate replacement is ordered after that removal and queued task ownership
cannot exceed the configured limit.

Real-time domain code stays on its dedicated callback or scheduler path and
must not submit blocking work there. A domain `Task::tick` may delegate to an
inherent method carrying that domain's real-time sanitizer annotation; the base
dispatcher adds no hidden work around the call. Heavy work crosses only the
bounded compute seam.

## Configuration document entry point

`WorkerConfigPatch` is the second way in: a configuration document types into it
and `apply` writes only `max_compute_tasks`, leaving whatever `cancel`,
`runtime`, and `pool` the builder already assembled standing. Those three
fields are wiring, not settings, and carry `#[patch(skip)]` for it.
`ComputePool` is what a document may say about the compute pool. It is
a section of its own rather than a field of `WorkerConfigPatch`, because `pool` is
skipped; the embedding surface owns the key it arrives under. `Shared` is
absent from it on purpose: it carries a live
`rayon::ThreadPool` only code can hand over, and a document has no way to name
one. `WorkerConfig::with_compute_pool` is how a decoded `ComputePool`
reaches a real `PoolConfig`. That mapping lives here, not at the construction
site, because `ComputePool` is `#[non_exhaustive]`: only this crate can
match it exhaustively, so only this crate may write the conversion — anywhere
else, the match would need a wildcard arm that silently swallows a variant
added later.
