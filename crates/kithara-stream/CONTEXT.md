# kithara-stream — Context

Contracts and invariants for the kithara-stream crate; the README is the overview.

## Architecture

`Peer::poll_next()` yields `FetchCmd`s to the `Downloader` (shared HTTP pool),
which writes chunks into a `StorageResource` and calls `on_complete()` on the peer;
`Stream<T>` (`Read + Seek`) reads that same resource through a `Source` impl
(`wait_range` / `read_at`). The halves meet only at the `StorageResource` — async
writes go through each `FetchCmd`'s self-contained `writer`, sync reads happen once
the range is present, no shared mutable state crosses. Cancellation flows top-down
through the cancel-token hierarchy in `crates/kithara-play/CONTEXT.md`.

## Read and Wait Policy

Two read paths, selected by the caller's statically-known context; `WaitMode` is
internal plumbing, never a public knob.

- Real-time worker: `try_read` / `probe_read` pass `Some(WAIT_RANGE_TIMEOUT)`
  (10ms) to `Source::wait_range` — one readiness probe, not a sleep; the backoff
  lives in the audio scheduler's `Waiting` park, so the produce core never blocks
  on a syscall.
- Off-RT consumer: `impl Read` passes `None` and parks event-driven until the range
  resolves. Deliberately **no** wall-clock budget here: give-up authority lives
  lower (downloader per-fetch inactivity timeout, cancel hierarchy), which can tell
  a slow-but-live transfer from a stall. A budget here would surface as
  `Interrupted`, which decoders misread as a seek; a real wedge is caught by the
  source's hang watchdog.

Outcome mapping in `try_read_with`: `SourceError::WaitBudgetExceeded` →
`Pending(NotReady(WaitBudgetExhausted))`; `WaitOutcome::Interrupted` →
`Pending(SeekPending)` while flushing, else `Pending(NotReady(WaitInterrupted))`; a
seek-epoch change at any checkpoint (before the wait, after the wait, after the
read) → `Pending(SeekPending)`; an empty `buf` → `Eof`, so callers must never probe
with a zero-length buffer. On the real-time probe path, a pull-driven segmented
source whose byte map resolves the current unit stops the wait range and read
slice at the end of that init or media segment. An oversized caller buffer
therefore receives a standard partial read from the ready current unit instead
of waiting on bytes from the next unit. The blocking adapter keeps the full
requested range: decoder construction must wait for every byte its initial read
touches and surface a terminal source error when one never arrives. Other
sources keep the full requested range in both modes. `impl Read` maps
`Bytes` → `Ok(n)`, `Eof` → `Ok(0)`, and re-loops on `NotReady`/`Retry` after
notifying the peer; `probe_read` never loops — it arms the peer wake and returns.
Both pending kinds surface as
`ErrorKind::Interrupted` carrying `StreamPending` (never `WouldBlock`) so
Symphonia's fragmented-MP4 reader treats the pause as transient; a variant fence
surfaces as `io::Error::other(VariantChangeError)`. Both payloads are
downcastable, so decoders recover the typed `PendingReason` without string
matching.

`Seek::seek` runs on the consumer thread and primes through `prime_seek_range`,
which returns immediately when `phase_at` already reports `Ready`/`Eof` (a re-seek
into resident bytes fires no cross-thread peer wake) and otherwise wakes the peer
once to re-aim the prefetch window, then blocks in `wait_range(_, None)`. The cursor
is published with `set_position` **before** priming: a peer re-aims by reading the
cursor, so priming ahead of it would wake the peer onto the old position and then
block on bytes nothing is fetching. Priming is advisory; `seek` re-checks
`source.len()` afterwards and restores the previous cursor when the target turns out
to be past EOF. `probe_seek` is the
real-time counterpart and never primes. The wake pair belongs to the source:
`peer_wake` is reader→peer (armed on the produce core, `notify_now` off-core),
`set_worker_wake` is peer→worker (fired from off-RT write/commit sites so an
underran worker re-ticks the instant bytes land); sources whose data is always
resident have neither. `take_reader_event_sink` must return a **fresh** sink on
every call — decoder recreation (ABR / format change) needs a clean state cursor.

`probe` is the narrow byte-space handle (`SourceProbe`): phase, cursor
(`position`/`set_position`), `len`, and `byte_map` behind an `Arc`, so a caller
that keeps `Stream` behind a mutex (the audio worker's shared stream) serves its
real-time polls and cursor moves without that lock — an off-thread holder parked
in a blocking `read`/`seek` would otherwise turn the poll into a wait.
Implementations answer from self-synchronizing state (HLS: the coord; file: the
shared inner) and must not take locks a reader wait can hold — the file probe's
`phase_at` and `len` may take the storage gate lock on the uncommitted path, a
short acquire the storage wait releases before parking, never one held across a
wait. `resolve_seek_target` is the shared cursor math: `Stream::seek` and the
probe-side `probe_seek` resolve a `SeekFrom` against the same position/length
answers.

## End-of-stream contract

`Stream::try_read` surfaces `StreamReadOutcome::Eof` only from a `Source` proving
the end is genuinely reached — `WaitOutcome::Eof` from `wait_range` or
`ReadOutcome::Eof` from `read_at`. A `Source` must **never** mint `Eof` for an
in-range range whose bytes have not arrived; that case is `WaitBudgetExceeded` /
`Pending(NotReady)` so the reader holds at need-data. A premature `Eof` for a
withheld in-range segment latches the audio consumer into `AtEof` and drives the
queue's silent auto-advance — pinned by
`tests/tests/kithara_queue/early_seek_size_withheld_advance.rs`.

- File (`FileSource`): `known_len` prefers `FileCoord::total_bytes()` (seeded from
  the announced `Content-Length`) over the reader's committed length. A range
  starting past that length is `Eof` only while the resource is `Committed`; on an
  `Active` resource it is `Waiting`.
- HLS (`HlsSource` / `HlsCoord`): EOF keys off the variant layout's published total
  size **gated by `sizes_complete()`** (every served segment's size known). While
  any served size is unknown the total is a lower bound and the gate holds
  `Pending`. See `crates/kithara-hls/CONTEXT.md` "Seek and wait_range Contract".

## Canonical Media Types

`AudioCodec`, `ContainerFormat`, and `MediaInfo` are defined here as the single
source of truth and re-exported by other crates. The conversions are the contract:
`AudioCodec → MediaInfo` (`From`, container filled only when the codec implies
one), `AudioCodec → ContainerFormat` (`TryFrom`, ambiguous codecs fail), `&[u8] → AudioCodec` (`TryFrom`, magic-prefix detection), `MediaInfo::parse_mime` (codec
*and* container from a `Content-Type`). `needs_exact_byte_sizes(codec, container)`
is the cross-crate size policy: only AAC (LC/HE/HEv2) and FLAC inside fMP4 tolerate
unknown byte sizes; everything else — including an unknown codec — requires exact
sizes. `kithara-decode` reads it when deciding whether a stream can open without a
complete size map.

## Downloader and Peers

`Downloader` is the sole `HttpClient` owner: created once at application level,
shared by `Clone` across protocol configs. `register(Arc<dyn Peer>)` returns a
`PeerHandle` whose cancel token fires when the last clone drops, letting the
registry release the peer entry. Peers are pull-driven — `Peer::poll_next` returns
`Poll<Option<Vec<FetchCmd>>>`; one-off requests go through `PeerHandle::execute`.
`FetchCmd::get(url)` / `head(url)` start its bon builder; each command owns its
`writer`, `on_complete`, and an optional epoch `CancelToken` combined by the
downloader with the track-level cancel.
Every streaming `FetchCmd` the registry accepts gets exactly one `on_complete` call:
`deliver` makes it for a fetch that ran (with or without a writer), and `Drop for Registry`
makes it for commands still queued when the downloader is cancelled. That callback is what
releases the command's claim — the HLS segment slot and the non-`Clone` `AssetWriter` both ride
in it — so a dropped closure strands a `<canonical>.tmp` that no one will ever write.
Imperative `execute`/`batch` commands carry no claim: their oneshot is the completion signal.

Scheduling routes each queued command into a 2×2 priority slot map keyed by
`(peer.priority(), command priority)`; the urgent slots (`High` peer or command)
drain fully before the demand slots each loop pass. A command's stamped priority
reflects the reader position at *emit* time, so `FetchCmd` also carries an
optional `DemandFn` — a live probe answering whether a reader currently blocks
on this command's bytes. `Registry::reschedule` (every loop pass) re-asks both
the peer priority and the demand probe, and an escalation moves the command to
the *front* of its more urgent slot: a prefetch the playhead caught up with
overtakes work stamped more urgent after it, instead of starving behind an
entire construction window while a reader waits (the UrgentDownSwitch hang).
Demand probes must be cheap, lock-free reads — they run on the download loop.
`reschedule` rebuilds every slot in one pass rather than patching by index, since a
slot can be both source and destination of the same pass. `RequestEnqueued` carries
the stamped priority; an escalation is a `reschedule` trace line, not a bus event.

`DownloaderConfig::for_client(client)` carries `abr_settings` for the shared ABR
controller, `demand_throttle`, `soft_timeout` (2s — publishes
`DownloaderEvent::LoadSlow` on the peer's bus without aborting the request),
`max_concurrent` (5 — global in-flight cap across all peers and command types), an
optional `runtime` handle, and an optional parent `cancel` (`Some` composes a child
scope, `None` owns a standalone one). Ownership sits above this crate: the
embedding surface (`kithara-app`, `kithara-ffi`) builds one `Downloader` and
threads it through `kithara-play::ResourceConfig::downloader`, so every peer shares
one HTTP pool; with none supplied, `kithara-play` builds a per-resource one.
`DownloaderConfigPatch` is the second entry point: a configuration document types into it
and `apply` writes past the builder, so a document can compose a value the builder would
have refused. `client`, `cancel` and `runtime` are wiring rather than settings and are
absent from it; `abr_settings` nests, so the ABR knobs are reached at
`downloader.abr_settings` and nowhere else — this crate owns the only `AbrSettings` the
embedding surface builds.

The downloader binds `AbrSettings::cancel` to its own scope before constructing
the shared controller, which therefore owns a child of the downloader scope. Downloader peer handles
retain a separate per-registration fetch/registry scope; ABR registration does not
receive that token and instead obtains the protocol track token through `Abr::cancel()`.
The downloader run loop also drives the controller's coalesced tick slots and nearest
`min_switch_interval` deadline inside `Registry::tick`. ABR does not spawn a second
worker or use an ambient runtime; its readiness participates in the same
cancellation-safe polling boundary as peer and registration readiness.

## Features

Default is `client-reqwest` + `tls-rustls`. `client-reqwest` / `client-wreq`
forward the HTTP backend to `kithara-net`, `kithara-events`, and `kithara-abr`;
`client-apple` forwards only to `kithara-net`; `tls-rustls` / `tls-native` forward
TLS selection. `probe` adds USDT probe points, `perf` hotpath instrumentation, and
`mock` exposes the `#[kithara::mock]` (unimock) trait mocks otherwise compiled only
under `cfg(test)`.

## Agent Guardrails

Keep `kithara-stream` generic: no HLS-, file-, or surface-specific policy in shared
contracts. `wait_range`, `read_at`, and the pull-driven `Peer` contract are this
crate's surface — fix the owned invariant, never paper over it.
