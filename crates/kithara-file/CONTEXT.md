# kithara-file — Context

Contracts and invariants for kithara-file; the README is the overview.

## Architecture

`FileConfig<S>::for_src(src)` -> `File<S>` (StreamType marker) -> internal `FileCoord`, which splits by
source: `FileSrc::Local` reads through `AssetStore<S>` with an absolute `ResourceKey`;
`FileSrc::Remote` runs an internal pull-driven `FilePeer` emitting `FetchCmd` batches to the
shared `dl::Downloader`. The `AssetStore<S>` owns one pending resource per `ResourceKey`: consumer
demand, the canonical reader/writer, and writer epochs stay in that storage state, while File
drives HTTP through a writer epoch capability. `FileSource` (impl `kithara_stream::Source`) wraps `FileCoord`; `Stream<File<S>>`
(`Read + Seek`) wraps `FileSource`. `FileSource` is synchronous: every async concern (HTTP fetch,
body streaming, finalization) belongs to the `Downloader` through `FilePeer`, and it holds the
`PeerHandle` from `Downloader::register` for its whole lifetime — dropping the last handle cancels
in-flight fetches.

`store: AssetStore<S>` and `pools: PoolRegion<S>` are both required configuration fields. They use
the same closed schema `S: HasPool<u8>`; `FileConfig<S>` keeps cheap clones of both. Remote source,
storage, and fallback transport therefore obtain byte buffers through one facade and one shared
hard budget. Local sources keep the same explicit contract even though they skip the downloader.
There is no global or lazily inferred pool.

## Reader contract

`Stream<File<S>>::Read + Seek` goes through `FileSource::wait_range` / `read_at`.

- `wait_range(_, Some(_))` is the audio-worker probe: checks phase once, returns
  `SourceError::WaitBudgetExceeded` for missing in-range bytes instead of blocking.
  `wait_range(_, None)` is the off-RT adapter path, delegating to the storage wait until bytes,
  EOF, failure, or cancel resolves. The source token is a per-wait cancel authority: it wakes a
  blocked adapter without cancelling the shared asset or another follower. A flushing seek
  short-circuits to `WaitOutcome::Interrupted` before any demand update.
- The probe clamps oversized read-ahead at a known length, so a fully cached file queried with
  `0..read_ahead` where `read_ahead > len` is `Ready`, not need-data.
- `Eof` requires **both** that the range starts at or past the known length **and** that the
  resource is `Committed`. An announced-but-uncommitted length yields `Waiting`; an in-range range
  not yet written returns `WaitBudgetExceeded` (→ `Pending` / need-data) so the reader holds
  rather than terminating. See `crates/kithara-stream/CONTEXT.md` "End-of-stream contract".
- Known length blends the announced total (`FileCoord::total_bytes`, seeded from
  response metadata) with the committed `AssetReader::len()`, announced first. When a commit has no
  explicit final length, the committed reader length is canonical. A ranged response
  takes its full total from `Content-Range`; its slice `Content-Length` is never treated as the
  resource total. If a server ignores the initial bounded range and returns a full `200` without
  `Content-Range`, that full response's `Content-Length` is the resource total.

## Fetch targeting

`FilePeer` targets one fetch at a time, and it targets it at the reader cursor
(`kithara_stream::Source::position`). `Stream<File<S>>::probe_seek` moves that cursor and arms the
peer precisely so the peer re-targets around the new position.

- The next fetch starts at the first gap **at or after** the cursor. Only when nothing is missing
  ahead of the cursor does the peer fall back to the first gap from byte 0. That second lookup is
  not a state-resolution fallback: both lookups are correct answers to different questions, and the
  order is the priority. The listener is blocked on the bytes under the cursor, so those come
  first; the earlier span still has to land for the resource to commit, so it is filled next. Drop
  the cursor lookup and a forward seek waits for the whole skipped span; drop the byte-0 lookup and
  a seeked-over span never lands and the resource never commits.
- A fetch streams forward from where it started, so a cursor that sits inside the fetch's span
  past the bytes it has landed can no longer be served by it — that fetch would deliver the whole
  skipped span first. The peer cancels it. Stored bytes decide where the fetch has got to (the
  first gap at or after its start), not a write offset the fetch publishes: that offset lands
  after `write_at`, and the landing is what wakes the reader, so a reader that consumed the bytes
  in between sat ahead of the offset without being ahead of the fetch and got its own fetch
  cancelled. Cancellation relinquishes the writer epoch, the completion path wakes the peer, and
  the next plan re-elects a writer and anchors on the new cursor. A fetch whose span ends before
  the cursor is a backfill: it starts behind the cursor by construction and never promised it
  anything, so it runs to completion.
- A plan that finds no gap over a known extent commits the resource from the peer. A cancelled
  fetch relinquishes without committing, and when it had already landed everything its replacement
  has nothing to fetch; parking there would leave every consumer waiting on a complete resource.
- The cursor steers nothing until the resource extent is known, and nothing once it sits outside
  it. A range request needs an extent: before the first response answers how long the resource is,
  a cursor past the end is indistinguishable from one inside it, and anchoring there would both ask
  for bytes that do not exist and cancel the one request that reports the real length. Both cases
  leave the head fetch to establish the resource. This is why a seek past the end of a not-yet-sized
  resource still terminates.
- Because fetches follow the cursor, stored bytes are sparse and `FileCoord::set_download_pos`
  reports the running fetch's write cursor, not a contiguous prefix.

## Sources

`FileSrc::Local(path)` requires an existing absolute path, opens it through `AssetStore` with an
absolute `ResourceKey`, skips all network activity, and yields a `FileSource` with no peer and no
downloader. Media bytes are read in place: never copied under the cache directory, and no layout
callback applies to that absolute key. Derived resources differ —
`kithara_play::ResourceConfig::asset_key` describes the same path as `AssetSource::Local`, selects
the `File` layout, and mints an ordinary relative key such as the default
`analysis/track.analysis`, so a custom `File` layout governs local-track analysis and other
derived artifacts while the original media file stays untouched. `FileSrc::Remote`:

- Opening returns as soon as the asset claim succeeds; `Content-Length` / `Content-Type` arrive
  later with the first response, so `len()` is `None` until then. `AcquisitionResult::Ready` means
  already committed (no remote fetch); `Pending` attaches a reader, resource lease, and optional elected
  writer to the `AssetStore`-owned session. Followers attach to that session and never acquire or
  reactivate the backing resource themselves.
- If a sibling `AssetStore` instance holds the atomic-chunked tmp for the same canonical path,
  `create` polls every 10 ms until that sibling commits or drops, or returns cancellation when
  its own work token fires. The loop is wrapped in
  `#[kithara::hang_watchdog]` and ticks the watchdog only while the tmp's length is *unchanged*,
  so a live sibling never panics and only a stale tmp from a crashed process does.
- Downloading is pull-driven and gap-driven: `Peer::poll_next` fetches from `next_gap(0, upper)`,
  the first missing byte from the start and never the seek position, so the landed prefix stays
  contiguous and `FileCoord::set_download_pos` is a true cached-prefix cursor. Backpressure runs
  through the consumer-demand entry shared between `FileCoord::read_pos_handle()` and the resource lease
  from `AssetStore::attach_pending_resource`, bounded by `look_ahead_bytes`. Before a missing range yields
  or blocks, File raises that consumer's monotonic requested-end floor; this immediate demand
  overrides the bounded prefetch frontier, including `look_ahead_bytes = 0`. The lease's one-shot
  reader Waker uses only `Weak<FileInner>`. On writes it rearms before waking the audio worker;
  on a terminal commit it settles file metadata and derived indexes before that wake. Completion
  work therefore stays outside the audio worker's real-time read path, and the Waker cannot pin a
  dropped source through the pending-resource state. Non-blocking audio probes wait for that
  commit and raise their demand to the whole resource, so the real-time decoder reads only the
  immutable committed snapshot. Blocking file readers may still consume published partial ranges.
- Only the **elected writer epoch** issues GETs. `poll_next` registers its peer Waker before the
  readiness/election check, leaves it armed on `Pending`, and clears that exact registration before
  `Ready`. A stale writer handle is removed and dropped outside the File mutex before the peer
  attempts promotion. Writer handoff changes only the epoch; the canonical writer and partial
  bytes stay in the same session, so two consumers of one URL share one GET whenever no handoff is
  needed.
- `FilePeer` holds `Weak<FileInner>`; `FileSource` owns the strong lifetime. Inside `FileInner`, the
  asset reader is declared before the demand lease, so the reader drops first. The last source drop
  synchronously retires the exact session, while the lease's `AssetStore` clone pins disk-checkpoint
  ownership through the remote lifetime.
- Each fetch uses a child of the writer cancel token. Source cancellation cancels only that fetch
  child; the peer holds non-owning wake guards for both source and session cancellation. Either
  cancellation wakes a parked peer before its next readiness check. Session cancellation ends the
  peer and forbids writer re-election. Source cancellation that races after a valid response has
  landed every advertised byte commits the canonical resource; otherwise it relinquishes only this
  source's epoch. `NetError::Cancelled` never emits `FileEvent::Error`. A fatal error, or an initial
  zero-progress error, fails the current epoch. Other transient completions keep partial coverage
  active, but may commit when every advertised byte already landed. Late stale callbacks cannot
  write, commit, fail, or publish File events for a successor. The peer clears its in-flight flag
  only after the completion callback settles the epoch, so a concurrent registry poll cannot issue
  a replacement GET during terminal cleanup.
- HTTP range upper bounds are exclusive internally and inclusive on the wire. Ranged responses
  must identify the requested start, interval, and numeric representation total with a valid
  `Content-Range`; a full `200` that
  ignores an initial range is accepted only with `Content-Length`. A resumed response without
  `Content-Range`, a mismatched interval, an unknown `*` representation total, or an unknown-size
  bounded full response fails before any byte is written.

## Configuration document entry point

`FileConfig<S>` is the one configuration struct this crate has: the tunables and the per-call
wiring live in it together. `#[derive(Patch)]` generates `FileConfigPatch` beside it, and `apply`
writes only the fields the document names, leaving the rest of `FileConfig` standing.
`extension: Option<String>` and `look_ahead_bytes: Option<u64>` are already optional on the
config, so a document names a plain value under `extension` / `look_ahead_bytes`, not
`Some(value)`. `tmp_claim_poll_interval` is read through
`serde(with = "humantime_serde::option")`, so a document writes `25ms` rather than a raw
millisecond count.

`kithara-app`'s `file:` section is that document's spelling. It types into `FileConfigPatch`,
`main` hands it to `AppConfig::file`, and every track is opened with that value:
`kithara_play::ResourceConfig` carries the patch — no `FileConfig` can exist before a track does —
and `kithara-play/src/resource/build.rs` applies it to the `FileConfig` it builds. The one field
the document does not have the last word on is `extension`: the file branch resolves it as the
per-call format hint, then the extension derived from the source path, then whatever the document
named — the two ahead of it identify the track being opened, which is more specific than a
crate-wide default.

`src`, `discriminator`, and `headers` are per-stream input, not crate-wide policy: they name what
is being fetched and how, not a tunable default. `store`, `pools`, `bus`, `cancel`, and `downloader`
are live handles or `S`-typed values a document has no way to name, the same reasoning
`kithara-hls::HlsConfig` applies to its own wiring fields.

Cache identity: naming is owned by the layout registered for the `File` marker in the shared
`AssetStore`. The stream binds `AssetSource::Remote { url, discriminator }` through
`store.scope::<File>()` and mints one `AssetResource::Source`; its extension comes from the
explicit `config.extension` hint, then the final URL-path extension, and finally `bin`, accepting
only a short all-ASCII-alphanumeric candidate, lower-cased. The default layout stores the file at
`<asset_root>/track/track.<ext>`, root hash over the canonical URL without query or fragment,
folding in the explicit discriminator when present. Query parameters are never an implicit
identity for direct files. A higher layer may register any `AssetLayout` for `File`;
`kithara_play::policy::QueryIdentityLayout` is the built-in domain-aware option selecting stable
identity parameters while ignoring rotating signatures and expiry values. Layouts are registered
once through `AssetStore::builder(pools.clone()).layouts`, and every `FileConfig<S>` holds cheap
clones of that same store handle and region.
