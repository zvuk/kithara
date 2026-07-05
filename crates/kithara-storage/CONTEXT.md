# kithara-storage — Context

Detailed contracts and invariants for the kithara-storage crate; the README is the overview.

## Blocking coordination

```mermaid
sequenceDiagram
    participant DL as Downloader (async)
    participant W as Writer
    participant SR as StorageResource
    participant RS as RangeSet
    participant G as CondvarGate
    participant R as Reader (sync)

    Note over DL,R: Async downloader and sync reader share StorageResource

    DL->>W: poll next chunk from network
    W->>SR: write_at(offset, bytes)
    SR->>RS: insert(offset..offset+len)
    SR->>G: notify_all()

    R->>SR: wait_range(pos..pos+len)
    SR->>RS: check coverage
    alt Range not ready
        SR->>G: wait()
        Note over SR,G: Park until bytes, commit, fail, reactivate, or cancel notify
        G-->>SR: woken by notify_all
        SR->>RS: re-check coverage
    end
    SR-->>R: WaitOutcome::Ready

    R->>SR: read_at(pos, buf)
    SR-->>R: bytes read
```

`wait_range` blocks the calling thread through `kithara_platform::sync::CondvarGate` until the requested byte range is fully written, or the resource reaches EOF/error/cancellation. The wait is event-driven; the 180s watchdog is a deadlock detector, not a polling interval.

## Mmap vs Mem

<table>
<tr><th>Aspect</th><th>MmapDriver</th><th>MemDriver</th></tr>
<tr><td>Backing</td><td><code>mmap-io::MemoryMappedFile</code></td><td><code>PooledOwned&lt;32, Vec&lt;u8&gt;&gt;</code> plus <code>ArcSwapOption&lt;Vec&lt;u8&gt;&gt;</code> committed snapshots</td></tr>
<tr><td>Lock-free fast path</td><td>Yes (<code>SegQueue</code> for write notifications)</td><td>No</td></tr>
<tr><td>Auto-resize</td><td>2x growth on overflow</td><td>Extend on write</td></tr>
<tr><td><code>path()</code></td><td><code>Some</code></td><td><code>None</code></td></tr>
</table>

## Chunked atomic claim

`AtomicChunked::open(canonical_path, factory)` opens a fresh chunked-atomic resource. The `factory` opens the inner resource at a given filesystem path; it is called once with the temp path during the constructor and once more with the canonical path after commit via `OpenIntent::Reopen`.

`open` atomically claims `<canonical>.tmp` via `OpenOptions::create_new`, so the filesystem rejects a second concurrent open of the same tmp path. It returns `StorageError::TmpClaimed` when another `AssetStore` instance (or process) is already writing the same canonical path; the caller should poll until the holder releases (commit or drop) and either retry or take a passthrough view once committed.

Stale temp left from a prior crashed run is not auto-wiped: liveness is signalled by tmp existence alone, so a leftover from `kill -9` blocks subsequent opens until cleaned up explicitly. The maintenance task is the caller's responsibility (a future enhancement may add PID-aware cleanup).

## Synchronization

Range tracking uses `RangeSet<u64>` (from `rangemap`) to record which byte ranges have been written. `wait_range` parks on the shared `CondvarGate` until a readiness transition notifies it, returns `Eof` when the resource is committed and the range starts beyond the final length, and returns typed storage errors for failure or cancellation. The `Interrupted` wait outcome is defined here for wrappers, but current production attribution lives in `kithara-assets` processing gates that convert cancellation/failure wakes into an interrupted read.

## In-place decorator lifecycle

The public `Resource<Active>` lifecycle is consume-self: `commit`, `reactivate`, and failure transitions move the writer handle so a second commit or write after commit is a compile error for external callers.

`commit_in_place`, `reactivate_in_place`, and `fail_in_place` are `pub(crate)` hooks only for the single-owner storage decorators (`Atomic` and `AtomicChunked`) that must rewrite a file in place: `OpenMode::ReadWrite` index files and the chunked-segment tmp/commit cycle. Those decorators own their writer exclusively and do not clone it, so the in-place transition does not create a second mutable owner or weaken the external consume-self contract.

## Notes on the `redundant_reexport` audit warning

`just audit kithara-storage` flags two `redundant_reexport` warnings — `MemOptions` and `MmapOptions` are surfaced *both* via `pub use` from `lib.rs` *and* via the `<Driver>::Options` associated type. The duplication is intentional and documented here per `AGENTS.md` ("if you must suppress, document why in the owning crate `CONTEXT.md`"):

- `MmapOptions` / `MemOptions` are the canonical user-facing constructor types — every caller writes `MmapOptions { path, initial_len, mode }` reachable via `kithara_storage::MmapOptions`.
- The `Driver::Options` associated type is the binding that lets the generic `Resource::<D>::open(token, opts: D::Options)` constructor pick the right driver from the option struct's type alone (via type inference at the call site). Removing `Options` would force every caller to pre-qualify with the driver type alias (`MmapResource::open` / `MemResource::open`) — a wider API ripple than the duplicated path is worth.
- Dropping the `pub use` (the audit's literal suggested fix) would break ~15 external callers that rely on `kithara_storage::{MmapOptions, MemOptions}`.

The two warnings are stable across releases; no other audit warnings should appear.
