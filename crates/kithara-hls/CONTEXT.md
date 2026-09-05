# kithara-hls — Context

Cross-file contracts for kithara-hls: facts no single file shows, that a wrong
design would violate. Field semantics, variant lists and playlist tags are
documented where they are declared; repo-wide rules belong to
[AGENTS.md](../../AGENTS.md), and encoded/container media types plus the
file/segment source contract to [kithara-stream](../kithara-stream/CONTEXT.md).

## Architecture

`HlsConfig<S>` -> `Hls<S>` (`StreamType`) -> `HlsCoord<S>`, which owns
`SessionSlots<S>` (active + optional incoming `HlsSession<S>` -> `HlsVariant<S>`),
`PlaylistCache<S>`, and `KeyStore<S>`. `HlsPeer<S>` (`impl dl::Peer`) drives the
coord and emits `FetchCmd` batches to `kithara-stream::dl::Downloader` ->
`AssetStore<S>`, which performs AES-128-CBC decryption (kithara-drm).
`HlsSource<S>` (`impl Source`) reads through the coord. None of these types is
public contract.

`store: AssetStore<S>` and `pools: PoolRegion<S>` share one closed schema
`S: HasPool<u8>` and one hard overall budget: HTTP client, playlist cache, key
store, segment storage, and temporary byte buffers all draw on it. There is no
global or per-component fallback pool — do not add one. `Hls::create` wraps the
master `cancel` token in a `CancelScope` (see `config.rs`); dropping `HlsSource`
cancels the scope and tears the peer down.

## Configuration document

`HlsConfig<S>` is this crate's one configuration struct — tunables and per-call
wiring together — and `#[derive(Patch)]` generates `HlsConfigPatch` beside it,
what a document's `hls:` section may say. Eight knobs travel: `size_probe_method`,
`download_batch_size`, `acquire_attempt_budget`, the three ephemeral-cache bounds,
`event_channel_capacity`, and `look_ahead_bytes`.

`net_options` is the one tunable deliberately skipped, so `hls.net_options` is
refused by name rather than parsed and dropped. An embedder holding a configuration
document already spells those options somewhere — `kithara-app` spells them in its
own top-level `net:` section — and two spellings of one value, one of them dead, is
what a document exists to prevent. The rest of the skipped fields are per-resource
identity (`url`, `base_url`, `discriminator`, `headers`, `initial_abr_mode`) or a
live handle only code can hand over (`store`, `pools`, `keys`, `bus`, `cancel`,
`downloader`).

## Sessions and Variant Switching

One authoritative **active** session plus at most one **incoming** session for a
pending ABR decision, published through `ArcSwap<ResidentSessions>`. `active()`
resolves the published ABR variant index and retries when the selector moves
under it, so a resolution never mixes snapshots.

The audio layer drives `VariantControl`. `plan_variant_reader(landing)` opens
**nothing** — only `prepare_variant_reader(plan, profile)` opens the incoming
session, builds an `OpenedVariantReader` and publishes both resident;
`take_prepared_variant_reader` hands it over, `promote_variant` / `abort_variant`
resolves.

`VariantTransition` identity is `(abr ticket, seek epoch)`. Ordinary transitions
retain the outgoing source; only the exact `UpSwitch { reason: EscapeStalled }`
claim marks it abandoned, and prepare re-validates that claim before audio may act
on it. Any mismatch discards the incoming session. Promotion runs inside
`commit_if_seek_epoch`, so a seek landing mid-promotion rolls the outgoing session
back instead of half-switching; it is deferred while the reader is untaken or the
ABR claim is `Locked`.

Lock cycle: `HlsSession::is_ready` is a pure question asked under the transition
lock and must not wake the peer — `wake_peer` takes the peer state lock while
`cancel_incoming_for_seek` takes the transition lock. Wake via
`wake_peer_for_readiness` after dropping the lock.

**No read fence:** no generation counter, read gate, or decoder acknowledgement.
A switch becomes visible on promotion, and its only published fact is the active
variant's `MediaInfo`. Reads are never short-circuited — `read_at` / `wait_range`
/ `phase_at` go through `HlsCoord::variant_serving`, which prefers the active
variant and otherwise falls back to any *shrunk* (`served_from > 0` or
`served_until < num_segments`) variant still serving its pre-switch byte range —
never an idle one at default served bounds. The audio layer detects a switch as any
format change (`kithara-audio` `pipeline::decode::format::detect`,
`pipeline::rebuild::policy::superseded`). Do not reintroduce a fence: it
duplicates a fact `MediaInfo` already carries.

### Fetch priority, budget, prefetch window

Owed fetches are tagged `RequestPriority::High` and drain first, so untagged
fetches queue behind every tagged one. Owed = the segment the audible session's
reader is stopped at, or every planned fetch of a not-yet-audible session.

`HlsPeer::poll_next` serves the active session first, reserves one slot for the
incoming session, and returns leftover budget to whichever still wants work; at
`prefetch_budget == 1` they alternate (`SessionTurns`).

`dispatch_constructing` caps an incoming session at `construction_segment_end`,
derived from the same window `reader_is_ready` waits on so the two cannot
disagree. `dispatch_from` anchors the look-ahead on
`max(session position, prefetch anchor)` and stops at the first segment past
`look_ahead_bytes` or `look_ahead_segments` (`Init` exempt). A
`construction_segment_end` bound disables both caps: the bound names a debt that
must land in full, look-ahead only trims optional prefetch, and an option must
never cut a debt.

`dispatch_from` publishes `prefetch_resume_at`, the cursor byte at which the
deferred segment enters the window, so `Source::advance` (`take_prefetch_resume`)
wakes the peer once per deferred segment, not once per read: the window is
cursor-anchored, so only the reader's consumption can make it stale and only that
consumption may re-open it. Popped-but-undispatchable non-terminal entries are
pushed back to the queue front, so an orphaned `Downloading` slot is re-claimed,
never dropped.

### Starving the transition, and not starving playback

While a transition builds, the outgoing session yields the downloader to it. An
audible session's prefetches past the owed window ride a rotating `lookahead`
cancel token — a child of the session's fetch token, so a seek's rearm burns both.
Installing the incoming slot (`prepare_planned_variant_reader`) retires that token:
queued packs deliver cancelled and in-flight ones abort, freeing capacity the
construction is otherwise starved of — those bytes lie past the cut the transition
latches and are dead to the splice anyway.

Until the transition resolves, `dispatch_active` holds the audible session to the
owed window (`dispatch_owed`), which is everything the splice still consumes —
pinned by `dispatch_owed_reaches_the_transition_latch`. A seek can park the
outgoing decoder's reads segments past the byte cursor; starving those fetches
parks the decoder forever while the incoming prime waits on its frontier just as
long. A retire that burned a fetch the wider window still owes is recovered by the
cancelled-settle requeue.

Retired and recovering fetches re-enter in plan order (`requeue_planned` is an
ordered insert): dispatch caps read the queue head, and a far look-ahead entry
parked at the front would wall off every nearer segment behind it. Pinned by the
`variant/flow/plan_queue.rs` unit tests.

### Reader demand escalation

Priority is stamped at emit and the `Downloading` claim dedupes re-emission, so a
prefetch stamped `Low` can never be re-issued as `High` once the reader catches up;
escalation goes through the slot instead. `wait_range` is the single filing site of
a parked read (RT `try_read` / `probe_wait` and the off-RT `Read` park all land
there): it calls `note_reader_demand()` on every claimed slot the read still needs
bytes from — the whole span, not the first gap, so the downloader serves them
concurrently. `build_cmd`'s live demand probe reads it back and `reschedule` walks
the command past later-stamped urgent work. Without this an urgent down-switch
starves the audible variant behind the whole construction window and the ring
drains dry.

Two sites must NOT file demand. `phase_at` stays a pure query — tracing, error
formatting and the transition's incoming-session probe observe it and must not arm
scheduling. The readiness poll of a session under construction
(`reader_is_ready` → `poll_range`) parks the same way, `wait_end` filing included,
but files no demand: it is not a read, the incoming variant's fetches are owed
already, and escalating its leftover look-ahead would put it back in front of the
audible variant.

Demand is filed by the same site as `ReaderRuntime::wait_end` but owned per slot:
a byte-space end alone cannot say which fetch a read waits on, and the escalation
must outlive the wait that filed it. A stale note surviving onto the next claim
is the tolerable side — clearing on claim would drop a legitimate one.

### Cancelled fetches settle back to the plan

A fetch cancelled in flight settles back toward the plan it was popped from, gated
by the plan revision, which supersedes on every rebuild — including a
short-circuit rebuild that changes nothing, because it still claims plan ownership.
`settle_cancelled` re-inserts only when the revision captured at pop is still
current and the entry is absent. A look-ahead retire rebuilds nothing, so its
cancels re-enter and the reader never waits on work nobody holds; a seek's rearm
rebuilds, so its cancels fail the compare and drop — the rebuilt plan owns the
re-dispatch, and a stale prefix must not resurrect behind the target.

### Wakes

When the downloader's `soft_timeout` marks an in-flight slot stalled it wakes the
peer immediately so `reconcile_escape` can move off that variant; a stalled reader
produces no progress wake. Reader progress reaches the peer through one forwarding
task delivering at most one wake before an ambient-aware cooperative yield:
draining that stream continuously would keep a Flash async participant active and
stop the virtual clock. Outside Flash it is the ordinary Tokio hand-off.

## Variant Init, Header Range, Probe Rebuild

`HlsVariant::build_init_entry` keys the init slot on the playlist `#EXT-X-MAP` URL
— a static fact — **never** on the init's known byte size; `variant/io/init.rs`
carries the failure mode. The init is enqueued at the queue front so the demuxer
has the container header before any media segment.

While declared but unsized (`init_size() == 0`), `read_at` on the fresh-activation
frame (`served_from() == 0`) holds reads pending until the commit sizes it — the
init prefix is reserved for the init, not media; a terminally failed init
(`init_failed`) releases the reservation so the read errors instead of hanging. A
switched-in variant's init is orphaned in natural space, so its reads are not
gated here.

`HlsVariant::header_byte_range` (alias `format_change_segment_range`) is the one
answer to "what does a demuxer re-read to re-establish container state after a
decoder-recreate": the virtual init range with an `#EXT-X-MAP`, otherwise segment
0's natural byte range for implicit-framing containers to re-scan, and
`Err(SourceError::FormatChangeNotApplicable)` on a shrunk variant.

`rebuild_with_decoder_probe` also enqueues `seg 0` when `from_seg > 0`: the decoder
factory's Symphonia probe needs the container header even with a separate init.
`HlsTrackState::apply_boundary_crossing` calls it only on the *aligned rescue* path
(active variant changed since the last poll, reader physically resolved to a
segment, `served_from() == 0`). A landing-anchored reader from
`HlsVariant::prepare_reader` gets no seg-0 probe and instead backs off one segment
behind the landing (`forward_segment - 1`).

## Byte-Space Layout

`Layout` (`variant/map/offsets.rs`) is the single coherent owner of every
cross-variant coordinate; its `Frame` / `FrameSnapshot` doc comments own the
publication rules. The caller-side rule: every layout read goes through
`try_published`, which returns `None` while a writer was in flight or the
publication changed, and a `None` means keep your gate closed and retry next tick
— never spin, never fall back to a partial read.

`layout_seek_invariant` is the one formula for "a layout reset would change nothing
worth a re-mint": canonical single-variant full-range geometry
(`is_canonical_complete`, every served size exact) with nothing parked in
`deferred_prefix`. A live seek tail alone does not force the reset — against a
canonical table it freezes nothing and only helps the EOF gate, so fully-cached
segment-aware seeks keep skipping the O(N) rebuild. It gates both
`HlsCoord::prepare_for_seek`'s reset call (a fully-cached seek must not touch the
layout at all — pinned by `stress_seek_audio`'s reset-counter assertion) and the
re-mint inside `reset_layout_to_full_range`; ABR invalidation and the reader wake
stay unconditional. `HlsCoord::reset_for_seek` is layout-only — it cancels no
in-flight body fetch.

### Post-seek frame freeze

While a segment-aware seek tail is live (`VariantSeek::segment_aware_tail !=
NO_SEEK_TAIL`, only for containers not needing exact byte sizes), the reader, the
demuxer's byte map and the peer's cursor all hold bytes minted on the frame the
seek anchored. A settle for a media segment behind the tail must not re-key
that space: `HlsVariant::apply_commit` parks its size in
`VariantSeek::deferred_prefix`, and the next re-mint
(`reset_layout_to_full_range`) retires the tail and applies the parked sizes
atomically with the fresh frame. While parked the real size lives only in
`deferred_prefix` and the size atom still reads placeholder — a transitional split
of truth that ends at that re-mint. Freeze and drain both run inside the `Layout`
write lock, so a settle can never park behind a tail whose space is already gone
and a parked size can never be lost or applied twice.

The init is **never** parked: init reads gate on its size being exact, so a parked
init starves every consumer waiting for its bytes (the demuxer probe of an ABR
pending variant deadlocks before activation could drain it). That same gate makes
the byte-space shift an init settle causes unobservable to a post-seek reader —
`init_read_at` / `init_contains` refuse until the size is exact, so the demuxer
cannot advance off the seek alias' anchor first. Optimistic init reads would
silently reopen the shift window. Pinned by
`post_seek_prefix_settle_does_not_move_the_frame` and neighbours in
`variant/tests.rs`.

## Seek Ownership

`prepare_for_seek` is the crate's `SeekPrepare` impl and the only site that
rebuilds the byte space for a seek. The control thread runs it once per seek
*before* the epoch is minted (`SeekHandle::begin`). Both halves take a lock —
`cancel_incoming_for_seek` the transition lock, `Layout::reset` the write lock plus
a copy of the offset table — so neither may run on the produce core, where the
reader resolves its anchor. Because the rebuild precedes the epoch, every observer
of that epoch already sees a matching layout and `ByteMap::anchor_at_time` stays a
pure query. Pinned by the `rtsan-hls` lane.

## EOF, Exact Sizes, Seek Aliases

Byte EOF is minted only when `total_bytes() > 0`, the offset is at or past it, and
`eof_ready()` holds; suppressed while a seek alias covers `range.start` or the
timeline is flushing. It is never inferred for an in-range segment whose body has
not arrived, nor while a served segment's size is unknown — either yields
`WaitBudgetExceeded` so the reader holds. An immediate seek before size estimation
completes can leave a served segment at size 0, where a premature `Eof` latches the
audio consumer into `AtEof` and skips the track. Pinned by
`tests/tests/kithara_queue/early_seek_size_withheld_advance.rs`.

`sizes_complete()` is MEDIA-only, so exact-seek short-circuits gate on
`all_sizes_complete()`; a media-only short-circuit would skip `SizeDemand::Init` and
the `complete_exact_seek_if_ready` anchor correction. A lazy size probe is allowed
only for a `SegmentContent::Plain` slot with a non-exact size — an encrypted
segment's transport length is not its plaintext length — so encrypted demands fall
back to a full body fetch pushed to the queue front.

The seek alias (`variant/flow/seqlock.rs`) is a lock-free base + resolved-exact
anchor pair under a generation tag: the produce core reads it on every
`find_at_offset`, off-RT resolvers publish the exact anchor under the base
generation so a stale resolver cannot attach to a newer alias, and retirement is
generation- or position-checked. `HlsVariant::seek_point_at_time` is the sole
`time → segment` mapping, so a variant switch and a plain seek to the same time
cannot diverge.

The fetch plan (`variant/flow/plan_queue.rs`) follows the same split: its
`fetch_is_planned`, reached from `phase_at` on the produce core, reads only the
lock-free membership mirror its module docs own.

## Seek and wait_range Contract

`Source::wait_range(start..end, timeout)` has two modes selected by `timeout`, and
`HlsVariant::wait_range` ignores the timeout *value*:

- **`Some(_)` — wake-free probe** (RT worker / `Stream::probe_read`): one
  non-blocking check that never sleeps, answering from the current virtual layout
  and otherwise returning `WaitBudgetExceeded` immediately.
- **`None` — event-driven blocking wait** (off-RT `Stream::read` /
  `prime_seek_range`): `HlsCoord::wait_range_blocking` parks on the shared readiness
  gate until the probe resolves, a covering segment fails, or cancel fires. **No
  wall-clock data poll.**

`HlsSessionReader` picks the blocking mode while the session's `ConstructionGate` is
armed (an off-RT decoder factory is building against it), the wake-free
`Some(Duration::ZERO)` probe otherwise.

`SizeSignal` (`src/signal.rs`) owns the readiness gate and the wakes; its doc
comments own the fire sites. Facts living nowhere else: the gate is single-waiter
(the one off-RT `wait_range(_, None)` reader) and a lock-free `ThreadGate`, not a
condvar, because RT-reachable edges signal it on the produce core; and for DRM the
decrypt gate opens only at commit, so `FetchSlot::settle` is the load-bearing wake,
not the byte writes. The reader's park is bounded (`READER_REAIM_INTERVAL`) and is
not a data poll — it returns `WaitBudgetExceeded` so an off-RT reader can re-aim a
mis-aimed peer. `WAIT_HANG_TIMEOUT` must exceed the kithara-net per-fetch total
timeout, so a stalled upstream fails terminally before the watchdog trips.

## Encryption (AES-128-CBC)

`KeyStore` resolves the `#EXT-X-KEY` URL and asks `KeyProcessorRegistry` for an
optional `PreparedKeyRequest`. The registry holds opaque resolvers and knows
nothing about domains or providers — domain matching and request shaping belong to
`kithara-play` policy. `#EXT-X-KEY` URIs resolve relative to the **segment** URL,
not the media-playlist URL. The original key URL owns memory and persistent-cache
identity; a prepared URL is used only for the wire request, and resolver
preparation happens inside the per-resource transaction after the cache rechecks,
so a cache hit never creates fresh salt or processor state. Session memory owns the
validated key that synchronous segment construction needs, so a cache persistence
failure does not turn a successful key fetch into a playback failure.

Decryption is part of the resource lifecycle, not a read-side step:
`DecryptProcessor` wraps a `DecryptContext` as a `ResourceProcessor` (identity =
`key||iv`), encrypted segments are acquired with `acquire_resource_with_ctx`, and
the store decrypts during the writer's commit before a reader becomes ready. Commit
must pass `Some(final_len)` — `None` silently skips decryption. PKCS7 unpad shrinks
the committed length below the announced size, so HEAD-based estimates are upper
bounds and the settle path adopts the committed `final_len`.

## Caching

Every playlist, init/media segment, and encryption key is an `AssetResource::Url`
in the `Hls` asset scope — playlists are cache resources like anything else, not a
side channel. Path and root naming belong to the layout registered for the
`Hls<S>` marker (kithara-assets `DefaultLayout` by default); this crate does not
own it, and there is no separate playlist or analysis layout.

Playlist and key bytes are one validated cache transaction
(`AtomicFetch::fetch_validated` under `AssetStore::with_resource_transaction`). A
cached body is parsed before acceptance; a validation failure or an empty committed
resource removes it via `AssetStore::remove_resource` and performs one network
fetch. Network bytes are validated **before** write-back, so an invalid response
never becomes a persistent cache hit. A valid body stays usable when persistence
fails — `PlaylistCache` keeps the parsed value for the rest of that instance.

`VariantSegments` holds the store's reader in exactly two slots keyed by
`ResourceKey` — init prefix and media segment, which is what one `read_at` walks —
so a decoder consuming a segment in small buffers opens the store once, not once
per buffer. Two slots is the bound because a held reader pins its asset; a reader
whose resource went `Failed` or `Cancelled` is dropped and reopened.

Eviction routes through an `EvictionSubscription` guard held by `HlsSource`:
`HlsCoord::broadcast_eviction` marks the lost key `Missing` on every variant that
owned it and rebuilds the active variant's queue from the reader's segment.
Non-active variants stay relaxed and pick their `Missing` entries up on the next
activation; `HlsVariant::on_evict` also releases the held resource.

## Integration

Composes with `kithara-audio` as `Audio<Stream<Hls<S>>>`; emits `HlsEvent` /
`DrmEvent` through a `DeferredBus` on the shared `EventBus`. Throughput estimation
and ABR decision policy live in `kithara-abr`; this crate only publishes
claims/commits and reports `Abr::progress`.
