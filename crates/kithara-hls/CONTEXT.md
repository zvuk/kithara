# kithara-hls — Context

Contracts and invariants for the kithara-hls crate; the README is the overview.

## Architecture

`HlsConfig` (bon, `start_fn = for_url`) → `Hls` (`StreamType`) → `HlsCoord`, which owns
`SessionSlots` (active + optional incoming `HlsSession` → `HlsVariant`: layout / queue / seek),
`PlaylistCache`, and `KeyStore`. `HlsPeer` (`impl dl::Peer`) drives the coord and emits `FetchCmd`
batches to `kithara-stream::dl::Downloader` → `AssetStore` (kithara-assets), which performs
AES-128-CBC decryption (kithara-drm); `HlsSource` (`impl Source`) reads through the coord.
`HlsCoord`, `HlsSession`, `HlsPeer`, and `HlsVariant` are internal, not contract; `VariantIndex` =
`usize`.

## Configuration

`store` is the only required non-`url` field. `look_ahead_bytes: None` resolves to
`HlsConfig::DEFAULT_LOOK_AHEAD_BYTES` (2 MiB) at the consumer site; `Some(0)` disables the cap.
`net_options` builds the internal HTTP client only when no `downloader` is injected; an injected
downloader carries its own. The ephemeral media prefetch window is `capacity - non_media_reserve`,
clamped to `[min_media_window, max(max_media_window, min_media_window)]` and capped by the store's
capacity. `cancel` is the master token: `Hls::create` wraps it in a `CancelScope` reaching
`HlsCoord`'s lock-free `is_cancelled()` on the produce core, downloader / net / asset paths derive
children, and dropping `HlsSource` cancels the scope and tears the peer down.

## Sessions and Variant Switching

One authoritative **active** session plus at most one **incoming** session for a pending ABR
decision, published through `ArcSwap<ResidentSessions>`. `active()` resolves the published ABR
variant index and retries when the selector moves under it, so a resolution never mixes snapshots.
`VariantControl`, driven by the audio layer: `plan_variant_reader(landing)` → `VariantReaderPlan`
(transition identity, target `MediaInfo`, landing time), opening **nothing** →
`prepare_variant_reader(plan, profile)` opens the incoming session, builds an `OpenedVariantReader`,
publishes both resident → `take_prepared_variant_reader` → `promote_variant` / `abort_variant`.

`VariantTransition` = `(abr ticket, seek epoch)` + active and incoming variant indices + the
outgoing disposition. Ordinary transitions retain the outgoing source. Only the exact
`UpSwitch { reason: EscapeStalled }` claim marks it abandoned, and prepare re-validates the same
claim before audio may act on that fact. Any mismatch discards the incoming session. Promotion runs
inside `commit_if_seek_epoch`, so a seek landing mid-promotion rolls the outgoing session back
instead of half-switching; it is deferred while the reader is untaken or the ABR claim is `Locked`.

`HlsSession::is_ready` is a pure question under the transition lock and must not wake the peer:
`wake_peer` takes the peer state lock, and `cancel_incoming_for_seek` takes the transition lock,
so a wake from here closes a lock cycle. Wake via `wake_peer_for_readiness` after dropping the
lock.

**No read fence:** no generation counter, read gate, or decoder acknowledgement. A switch becomes
visible on promotion, and its only published fact is the active variant's `MediaInfo`. Reads are
never short-circuited — `read_at` / `wait_range` / `phase_at` go through
`HlsCoord::variant_serving`, which prefers the active variant and otherwise falls back to any
*shrunk* (`served_from > 0` or `served_until < num_segments`) variant still serving its pre-switch
byte range; idle variants with default served bounds are excluded. The audio layer detects it as any
format change — cached `MediaInfo` vs the source's current one (`kithara-audio`
`pipeline::decode::format::detect`, `pipeline::rebuild::policy::superseded`); do not reintroduce a
fence, it duplicates a fact `MediaInfo` already carries.

Fetch priority, budget, prefetch window. Owed/construction fetches are tagged
`RequestPriority::High` and drain first, so untagged fetches queue behind every tagged one; owed =
the segment the audible session's reader is stopped at, or every planned fetch for a not-yet-audible
session. `HlsPeer::poll_next` serves the active session first, reserves one slot for the incoming
session, and returns leftover budget to whichever still wants work; at `prefetch_budget == 1` they
alternate (`SessionTurns`). `dispatch_constructing` caps an incoming session at
`construction_segment_end`, derived from the same window `reader_is_ready` waits on so the two
cannot disagree; the cap ends when the reader is transferred — a priming decoder must not stay
inside a build-sized window. `dispatch_from` anchors the look-ahead on
`max(session position, prefetch anchor)` and stops at the first segment past `look_ahead_bytes` or
`look_ahead_segments`, with `Init` exempt from both; it publishes `prefetch_resume_at`, the cursor
byte at which the deferred segment enters the window, so `Source::advance` (`take_prefetch_resume`)
wakes the peer once per deferred segment rather than once per read — the window is cursor-anchored,
so only the reader's consumption can make it stale and only that consumption may re-open it.
Popped-but-undispatchable non-terminal entries are pushed back to the queue front, so an orphaned
`Downloading` slot is re-claimed, never dropped.

While a variant transition is building, the outgoing session yields the downloader to the
construction. An audible session's prefetches past the owed window (the playing segment and the
next) ride a rotating `lookahead` cancel token — a child of the session's fetch token, so a seek's
rearm still burns both. Installing the incoming slot (`prepare_planned_variant_reader`) retires
that token: queued look-ahead packs deliver cancelled and in-flight ones abort, freeing capacity
the construction is otherwise starved of — those bytes lie past the cut the transition latches and
are dead to the splice anyway. Until the transition resolves, `dispatch_active` holds the audible
session to the same owed window (`dispatch_owed`), or the next poll would refill the look-ahead.
Retired and recovering fetches re-enter the plan in plan order (`requeue_planned` is an ordered
insert): dispatch caps read the queue head, and a far look-ahead entry parked at the front would
wall off every nearer segment behind it.

When the downloader's `soft_timeout` marks an in-flight slot stalled, it wakes the peer immediately
so `reconcile_escape` can move away from that variant; a stalled reader produces no progress wake.
Reader progress reaches the downloader peer through one forwarding task. It delivers at most one
wake before an ambient-aware cooperative yield: a producer may publish another edge while the task
is still being polled, and draining that stream continuously would keep a Flash async participant
active and prevent the virtual clock from advancing. Outside Flash the same yield is the ordinary
Tokio scheduler hand-off; it does not delay or coalesce the reader edge.

## Variant Init, Header Range, Probe Rebuild

`HlsVariant::build_init_entry` (`variant/io/init.rs`) records the init slot as
`Option<Segment::Init>`, keyed on the playlist `#EXT-X-MAP` URL — a static fact — **never** on the
init's known byte size. `None` means no `#EXT-X-MAP` or a byte-range-embedded init (inside segment
0's byte range): `rebuild` never enqueues `PlannedFetch::Init` and every init query reads 0.
`Some(init)` is a real `#EXT-X-MAP` init, always with a URL; its size starts non-exact
(`INIT_PLACEHOLDER_BYTES` = 16 KiB) for containers not needing exact byte sizes and unknown
otherwise, until the commit or a lazy size probe publishes the real length, and it is enqueued at
the queue front so the demuxer has the container header before any media segment.

While declared but unsized (`init_size() == 0`), `read_at` on the fresh-activation frame
(`served_from() == 0`) holds reads pending until the commit sizes it — the init prefix is reserved
for the init, not media; a terminally failed init (`init_failed`) releases the reservation so the
read errors instead of hanging. A switched-in variant's init is orphaned in natural space
(`init_descriptor_at` returns `None` when `served_from() != 0`), so its reads are not gated here.

`HlsVariant::header_byte_range` (alias `format_change_segment_range`) returns the range a demuxer
reads to re-establish container state after a decoder-recreate: `served_from() != 0` →
`Err(SourceError::FormatChangeNotApplicable)`; with an `#EXT-X-MAP` init → the virtual init range
`0..init_size`; otherwise → segment 0's natural byte range, from which implicit-framing containers
(ADTS, MP3, MPEG-TS) re-scan.

`rebuild_with_decoder_probe` also enqueues `seg 0` when `from_seg > 0`, so the decoder factory's
Symphonia probe has the container header, required even with a separate init. Called from
`HlsTrackState::apply_boundary_crossing` only on the *aligned rescue* path: active variant changed
since the last poll, reader physically resolved to a segment, `served_from() == 0`. A
landing-anchored reader from `HlsVariant::prepare_reader` gets no seg-0 probe (it seeks straight to
the landing anchor) and instead backs off one segment behind the landing (`forward_segment - 1`)
unconditionally.

## Byte-Space Layout

`Layout` (`variant/map/offsets.rs`) is the coherent owner of the cross-variant coordinates
`offsets`, `byte_shift`, `served_from`, `served_until`, `init_seed`, all in one immutable `Frame`
published through `ArcSwap`, so a reader can never mix the shift of one activation with the served
bounds of the next. Readers `load` the frame lock-free and allocation-free; writers serialize via
`write_lock` (load → clone → mutate → store), since `ArcSwap` alone lets a concurrent
read-modify-write lose an update. `total` and `sizes_complete` are lock-free atomics republished
from one `FrameSnapshot` at the tail of every mutation, so byte-EOF gates never mix them across
mutations. `publication_seq` is a seqlock (odd while publishing) and every layout read goes through
`try_published`, which returns `None` when a writer was in flight or the publication changed —
callers then keep their gate closed and retry next tick rather than spinning.

`is_canonical_complete` (behind `layout_seek_invariant`) is true when the frame is already canonical
single-variant full-range geometry *and* every served size is exact; `HlsCoord::prepare_for_seek`
then skips the O(N) layout reset, while ABR invalidation and the reader wake stay unconditional.
`HlsCoord::reset_for_seek` is layout-only (`reset_layout_to_full_range`) and does not cancel
in-flight body fetches; `HlsVariant::reset_to_full_range` (from `prepare_reader`) also clears the
seek alias, the exact-seek/exact-byte demands, and the segment-aware tail.

## Seek Ownership

`prepare_for_seek` is the crate's `SeekPrepare` impl and the only site that rebuilds the byte space
for a seek. The control thread runs it once per seek *before* the epoch is minted
(`SeekHandle::begin`). Both halves take a lock — `cancel_incoming_for_seek` the transition lock,
`Layout::reset` the write lock plus a copy of the offset table — so neither may run on the produce
core, where the reader resolves its anchor. Because the rebuild precedes the epoch, every observer
of that epoch already sees a matching layout and `ByteMap::anchor_at_time` stays a pure query;
`HlsPeer::apply_seek_change` then repositions only the peer's own next-segment state. Pinned by the
`rtsan-hls` lane.

## EOF, Exact Sizes, Seek Aliases

Byte EOF is minted only when `total_bytes() > 0`, the offset is at or past it, and `eof_ready()`
holds — `sizes_complete()` **or** `segment_aware_seek_tail_complete()` (every segment from the seek
tail onward exact, for containers not needing exact byte sizes). Suppressed while a seek alias
covers `range.start` and while the timeline is flushing. It is never inferred for an in-range
segment whose body has not arrived, nor while a served segment's size is unknown — either yields
`WaitBudgetExceeded` (→ `Pending`/need-data) so the reader holds. Sizes are normally learned up
front (`#EXT-X-BYTERANGE`, or a `Content-Length` / `Content-Range` probe), but an immediate seek
before size estimation completes can leave a served segment at size 0, where a premature `Eof` would
latch the audio consumer into `AtEof` and skip the track. Pinned by
`tests/tests/kithara_queue/early_seek_size_withheld_advance.rs`.

`sizes_complete()` is MEDIA-only, so exact-seek short-circuits gate on `all_sizes_complete()` =
`sizes_complete() && exact_init_complete()`; a media-only short-circuit would skip
`SizeDemand::Init` and the `complete_exact_seek_if_ready` anchor correction. Raw WAV/PCM has no
`#EXT-X-MAP`, so `exact_init_complete()` is vacuously true. Exact-size demands (`SizeDemand::Init` /
`Segment(idx)`) are deduplicated in `SizeDemandState` (queued + inflight sets); a probe is allowed
only for a `SegmentContent::Plain` slot with a non-exact size, since an encrypted segment's
transport length is not its plaintext length, so encrypted demands fall back to a full body fetch
pushed to the queue front. `set_exact_seek_demand` / `set_exact_byte_seek_demand` no-op for
containers not needing exact byte sizes, and clear when everything is already exact.

The seek alias (`variant/flow/seqlock.rs`) is a lock-free base + resolved-exact anchor pair under a
generation tag: the produce core reads it on every `find_at_offset`, off-RT resolvers publish the
exact anchor under the base generation so a stale resolver cannot attach to a newer alias, and
retirement is generation-checked (`retire_seek_projection`) or position-checked
(`retire_seek_projection_if_moved` from `advance`). `HlsVariant::seek_point_at_time`
(`variant/map/media.rs`, bisect over the segment decode-time table) is the sole `time → segment`
mapping, so a variant switch and a plain seek to the same time cannot diverge.

The fetch plan (`variant/flow/plan_queue.rs`) follows the same produce-core split: the deque is
mutated under its mutex by planner sites only, every mutation updates a lock-free membership
mirror (atomics) while still holding the lock, and `fetch_is_planned` — reached from `phase_at` on
the produce core — reads only the mirror, never the lock. A membership answer may be a mutation
early or late relative to the deque, the same temporal slack a racing lock acquisition always had.

## Seek and wait_range Contract

`Source::wait_range(start..end, timeout)` has two modes selected by `timeout`:

- **`Some(_)` — wake-free probe** (RT worker / `Stream::probe_read`): one non-blocking check, never
  sleeping — `Ready` when the bytes are readable in the current virtual layout, `Interrupted` while
  flushing, `Eof` past total bytes, terminal `Err` when a covering segment failed, else immediate
  `WaitBudgetExceeded`. `HlsVariant::wait_range` ignores the timeout value.
- **`None` — event-driven blocking wait** (off-RT `Stream::read` / `prime_seek_range`):
  `HlsCoord::wait_range_blocking` parks on the shared readiness gate until the probe resolves, a
  covering segment fails, or cancel fires. **No wall-clock data poll.**

`HlsSessionReader` picks the blocking mode while the session's `ConstructionGate` is armed (an
off-RT decoder factory is building against it), the wake-free `Some(Duration::ZERO)` probe
otherwise.

`SizeSignal` (`src/signal.rs`) pairs the readiness gate with the late-bound audio-worker wake and
the peer wakes. The gate is a lock-free `kithara_platform::sync::ThreadGate` (atomic bump +
`unpark`), not a condvar, because RT-reachable readiness edges signal it on the produce core, which
must not take a condvar mutex. Single-waiter — the one off-RT `wait_range(_, None)` reader. `fire()`
signals the gate **and** re-ticks the audio worker; it is fired from every off-RT site where new
bytes or a resolvable range appear, including each segment byte write, each terminal settle
(`FetchSlot::settle` commit/fail/cancel — for DRM the decrypt gate opens only at commit, so settle
is the load-bearing wake), and `prepare_for_seek`. Cancel is the one transition with no
producer-side signal: the wait registers a `CancelToken::on_cancel` waker for its own lifetime.

The reader snapshots the gate counter **before** probing and parks only if it is unchanged — a
seqlock guard closing the lost-wakeup window, since the probe predicate and the gate sit under
different locks. The park is bounded by `READER_REAIM_INTERVAL` (25 ms) and is not a data poll: the
wait returns `WaitBudgetExceeded` so the off-RT reader can re-assert a possibly mis-aimed peer and
re-enter. A genuine wedge trips `#[kithara::hang_watchdog]` (`WAIT_HANG_TIMEOUT` = 180 s, which must
exceed the kithara-net per-fetch total timeout so a stalled upstream fails as a terminal `Err`
first). The worker wake (`Source::set_worker_wake`, installed by the audio worker) is `None` until
the worker exists; the audio scheduler's 10 ms `Waiting` park is the backstop for that window.

## Encryption (AES-128-CBC)

Encrypted segments parse `#EXT-X-KEY`; `KeyStore` resolves the key URL and asks
`KeyProcessorRegistry` for an optional `PreparedKeyRequest`. The registry holds opaque resolvers and
knows nothing about domains or providers — domain matching and request shaping belong to
`kithara-play` policy. `#EXT-X-KEY` URIs resolve relative to the **segment** URL, not the
media-playlist URL. The original key URL owns memory and persistent-cache identity; a prepared URL
is used only for the wire request, and resolver preparation happens inside the per-resource
transaction after memory and persistent-cache rechecks, so a cache hit never creates fresh salt or
processor state. Every final AES-128 key, used directly or produced by a processor, is validated as
exactly 16 bytes before entering session memory or the asset store. Key repair is serialized per
resource: an invalid cached key is removed and refetched once, cache state rechecked after the
transaction is acquired. Session memory owns the validated key needed by synchronous segment
construction, so a cache persistence failure does not turn a successful key fetch into a playback
failure.

Decryption is part of the resource lifecycle, not a read-side step: `DecryptProcessor` wraps a
`DecryptContext` as a `ResourceProcessor` (identity = `key||iv`), encrypted segments are acquired
with `acquire_resource_with_ctx`, and the store decrypts during the writer's commit before a reader
becomes ready. Commit must pass `Some(final_len)` — `None` silently skips decryption. PKCS7 unpad
shrinks the committed length below the announced size, so HEAD-based estimates are upper bounds and
the settle path adopts the committed `final_len`.

## Caching

Every playlist, init/media segment, and encryption key is an `AssetResource::Url` in the `Hls` asset
scope — playlists are cache resources like anything else, not a side channel. `stream/hls.rs` mints
the scope once via `store.scope::<Hls>(&AssetSource::Remote { url, discriminator })`; each semantic
site calls `scope.key(&resource)` once and reuses the minted key. Path and root naming belong to the
layout registered for the `Hls` marker (via `AssetStore::builder().layouts`, default kithara-assets
`DefaultLayout`); this crate does not own it, and there is no separate playlist or analysis layout.

Playlist and key bytes are one validated cache transaction (`AtomicFetch::fetch_validated`, run
under `AssetStore::with_resource_transaction` so the store serializes it per resource across
independent caches). A cached body is parsed/validated before acceptance; a validation failure or an
empty committed resource removes the resource via `AssetStore::remove_resource` (which also clears
its availability-index entry) and performs one network fetch. Network bytes are validated **before**
write-back, so an invalid response is returned as an error and never becomes a persistent cache hit.
A valid network body stays usable when persistence fails: the failure is logged at `WARN` and a
later session retries persistence, while `PlaylistCache` keeps the parsed value in memory
(`OnceCell` per master / per variant) for the rest of that instance.

Eviction is routed through an `EvictionSubscription` guard held by `HlsSource`:
`HlsCoord::broadcast_eviction` marks the lost key `Missing` on every variant that owned it and
rebuilds the active variant's queue from the reader's segment. Non-active variants stay relaxed and
pick the `Missing` entries up on their next activation.

## Integration

Composes with `kithara-audio` as `Audio<Stream<Hls>>`; emits `HlsEvent` / `DrmEvent` through a
`DeferredBus` on the shared `EventBus`. Throughput estimation and ABR decision policy live in
`kithara-abr`; this crate only publishes claims/commits and reports `Abr::progress`. The
`client-*` / `tls-*` features forward the HTTP backend and TLS selection to every network-reaching
dependency.
