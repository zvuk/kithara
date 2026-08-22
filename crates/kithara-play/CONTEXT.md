# kithara-play - Context

Contracts and invariants for the kithara-play crate; the README is the overview.

## Planes & Ownership

- `api/` - stable public shapes; `bridge/` - cross-plane protocol and shared RT handles
  (`NodeInputs` / `SlotControl`, `PlaybackShared`, `SharedEq`, `PlayerCmd`, `PlayerNotification`),
  plus a re-export of the session protocol.
- `policy/` - domain-aware cache identity and DRM key-request routing.
- `resource/` - source detection, `ResourceConfig`, reader construction.
- `player/` - playlist and parameter state in `state/`, transitions in `flow/`.
- `engine/` - session registration, slot table, mix batch.
- `rt/` - lock-free Firewheel nodes; per-track state under `rt/track/`.
- `session/` - protocol, state, graph dispatch, platform clients.
- `wasm` - target-gated browser binding surface.

`player`, `engine`, and `session` are intentional orchestration planes (their entry files bind API
state, RT controls, and session commands), so `.config/arch/thresholds.toml` raises
`module_fan_out` to 9 for this crate; do not add re-export hops solely to lower that count.

## Domain Policies

`policy` is the orchestration-level owner of domain matching - not `kithara-assets`,
`kithara-platform`, or `kithara-drm`. `DomainPattern` supports exact hosts, `*.domain` subdomains,
and `*`; host matching is case-insensitive, rules are ordered, first match wins.

`QueryIdentityLayout` implements the ordinary `AssetLayout` contract; unmatched resources delegate
to `DefaultLayout`. A `QueryIdentityRule` lists application-defined, **case-sensitive** query keys
for one or more domain patterns. Configured values contribute to the asset-root identity in
declared-key order; repeated values retain URL order; missing / empty / repeated values stay
distinct. An existing `AssetSource::Remote` discriminator is combined with the query identity,
never replaced. Unlisted signatures, expiry values, fragments, and query-pair ordering do not
fragment the cache, and raw query text never reaches a path (`DefaultLayout` hashes it into the
leaf). The same layout can be registered normally for `File`, `Hls`, or both.

`DomainKeyPolicy` implements `KeyRequestResolver` and is registered normally in the DRM
`KeyProcessorRegistry`. A matching `DomainKeyRule` merges its static headers and query parameters
with fresh request-factory output into one `PreparedKeyRequest`; factory headers win over static
ones. The opaque DRM registry exposes neither domain rules nor resource headers, so callers that
also need playlist/segment headers keep the same immutable `Arc<DomainKeyPolicy>` and ask it via
`resource_headers` - never a second policy source of truth.

## Tempo & Key-Lock

`kithara_audio::StretchControls` (one `Arc` per deck, in `PlayerConfig.timestretch`) is the single
source of truth for playback speed, shared between the UI and the worker effect chain, which reads
it each chunk. It always carries `speed` + `region_plan`; with a stretch backend compiled in
(`kithara-audio`'s `stretch-signalsmith` / `stretch-bungee`, native targets only) it also carries
`keylock` and `backend`. Rate setters (`PlayerImpl::set_rate`, `play`) write this one handle - no
second rate atomic, no manual mirror. `Queue` delegates `set_rate` to the player; key-lock and
backend are set by the consumer directly on the shared handle. `prepare_config` always passes the
shared controls into every track (`stretch = Some(..)`). With a backend compiled in the effect
chain runs a source-domain `TimeStretchProcessor` at `ratio = 1/speed`, and:

- **key-lock off** (the constructed default): `pitch = speed` - speed shifts pitch, vinyl-style.
- **key-lock on**: `pitch = 1.0` - speed preserves pitch.

At speed 1.0 with no region plan the slot bypasses. Without a backend - including every wasm build,
where it is cfg'd out regardless of features - no speed DSP is inserted and PCM output stays pinned
to 1.0. Because the controls are read each chunk, **speed, key-lock, and backend all apply live,
mid-track - no reload.** Switching backend rebuilds the DSP backend; returning to unity passthrough
resets buffered stretch state.

Fixed-ratio sample-rate conversion is a separate stage: Apple fused builds use the codec-embedded
placement, other builds the standalone decode-adapter resampler.

## Live Equalizer Layout

`PlayerImpl::set_eq_layout` replaces one player's master EQ while the player is
running. The session graph is the actuator: it builds the replacement node on
the control thread, reconnects every existing slot through it to the unchanged
master-volume node, removes the old EQ, and submits one graph update. The audio
thread never allocates, locks, or reconstructs filters for a layout change.

`EngineImpl` owns the current `EqBandConfig` vector before registration and uses
it for `eq_band_count`; after registration the session's `PlayerState` owns the
live graph projection. `SharedEq` is the control-plane gain mirror shared by
the session and slot handles; no audio processor reads it, and the DSP takes its
gains from the session's node event queue instead. Reading and writing one gain
are lock-free atomics. Layout replacement swaps the whole band array behind the
`ArcSwap` every handle clone points at, so existing slot handles observe the new
band count. Gains embedded in the replacement layout become the new live gains.

## Events

One `kithara_events::EventBus` (tokio broadcast) per player. `player.subscribe()` and
`engine.subscribe()` return receivers on the *same* bus - the player's bus **is** the engine's bus.
The `PlayerEvent` / `ItemEvent` / `EngineEvent` / `SessionEvent` / `DjEvent` enums and the
`PlayerStatus`, `TimeControlStatus`, `ItemStatus`, `WaitingReason`, `SlotId` types are owned by
`kithara-events` and re-exported here.

`SessionDuckingMode` is owned by this crate and maps `Off` / `Soft` / `Hard` to session-output
gains `1.0` / `0.4` / `0.2`.

## Queue Auto-Advance

`PlayerImpl` exposes a handover API for external orchestrators and internal tests:

- `arm_next(idx) -> Option<Arc<str>>` - load the next item into the audio thread, ready for gapless
  stitch (cf=0) or parallel fade (cf>0). Idempotent per index; a differently-armed index is
  unloaded first; `None` when the slot is empty/out of range.
- `commit_next(idx) -> Result<(), PlayError>` - promote the armed slot (cf>0 only; the audio thread
  handles cf=0 internally). `NotReady` when nothing is armed, `ArmIndexMismatch` on index
  disagreement.
- `unarm_next()` - drop the armed slot without committing; skips the unload when that slot is
  already activated for the current index (it is the leading track).
- `armed_next() -> Option<usize>` - the armed, not-yet-activated index.

Two near-end triggers are published: `PlayerEvent::PrefetchRequested` and
`PlayerEvent::HandoverRequested` (emitted only when `crossfade_duration > 0`).
`PlayerConfig::auto_advance_enabled` (default `true`) uses a built-in linear policy
(`next = current + 1`). `kithara-queue::Queue` disables that built-in policy and reacts to
`HandoverRequested` by selecting the loaded successor via `select_item_with_crossfade`; it does not
call `arm_next` / `commit_next`.

`select_item_with_crossfade` fails with `PlayError::ItemConsumed` before any bookkeeping when the
target index is neither armed, nor the already-announced current item, nor still holding a resource

- the UI must not drift from the audio.

## Engine Lifecycle

`start()` -> `allocate_slot()` -> attach `PlayerImpl` -> load the current item -> `play()`. Tear
down with `release_slot(id)` then `stop()`.

`EngineImpl::start` is **atomic single-start**: the `running` check-then-act is serialized by an
internal `start_lock`, and `running` flips to `true` only after `session.start_player` has fully
succeeded. Two concurrent starters cannot both dispatch `start_player`: the loser observes
`running == true` under the lock and returns `EngineAlreadyRunning`. `ensure_engine_started` treats
`EngineAlreadyRunning` as success, so a concurrent start is idempotent, never a session desync.

`stop_player` releases the output device once no player is left started: the Firewheel context,
session output node, and limiter are torn down and the next `start_player` builds a fresh context.

Drop order is load-bearing. `PlayerImpl` declares `phase` before `core`, and `PlayerCore` declares
`items` before `engine`, so undelivered resources (which hold worker references) release before the
engine's `Drop` shuts the worker down.

## Cancel Hierarchy

Cancel is the typed propagate-down tree from `kithara-platform` (`common/cancel/`).

`PlayerImpl` derives its token through `CancelScope::new(config.cancel)`: a passed `CancelToken`
(consumer crates `Queue` / `App` / FFI mint their own root and pass a child through
`PlayerConfig.cancel`) makes the token a child of it; `None` makes it a fresh root. The same token
is handed to `EngineConfig.cancel`, which scopes the audio worker. `prepare_config` gives every
track a `.child()` of it, and subsystems (Downloader, AssetStore, HlsPeer, epoch cancel) derive
further children.

`CancelScope::Drop` is **passive**: dropping a scope cancels nothing. Teardown is explicit -
`PlayerImpl::drop` calls `engine.cancel()`, cancelling the player's own subtree and never the
potentially-foreign parent it was handed.

`Resource` holds a `CancelGuard` around the per-track token, declared before `inner` so a
mid-session unload tears down the whole track subtree (stream *and* `Audio`) before the reader
drops. The `From<Resource>` reader unwrap disarms the guard, because the live reader then outlives
the wrapper (analysis worker) and rides the analysis run-scope cancel instead.

Hard-coded `CancelToken::root()` / `CancelToken::never()` outside the `cancel_root_sites` allowlist
are forbidden and enforced by `just lint arch`; kithara-play is not on that allowlist.

## Real-Time Audio Thread

Four Firewheel processors run on the audio thread and carry
`#[kithara::rtsan_forbid_blocking]`: `PlayerNodeProcessor::process` (`rt/processor.rs`),
`MasterEqProcessor::process` (`rt/eq.rs`), `LimiterProcessor::process` (`rt/limiter.rs`), and
`TapProcessor::process` (`rt/tap.rs`, present while a mix tap is enabled). They stay
allocation-, free-, and lock-free: render scratch is sized in `new_stream`
(`RenderPass::resize`), which Firewheel calls on the main thread, and finished or evicted tracks go
to the bounded trash ring from `bridge::slot_channels` instead of being freed on the audio thread -
the main thread drains it in `PlayerImpl::process_notifications`. A full command ring surfaces as
`PlayError::SlotChannelFull`, never as a block.

`render_audio` clamps its block to the scratch it holds, so a host exceeding the `max_block_frames`
it declared loses that block's tail rather than allocating in the callback.

Pause and resume are ramps. `RenderPass` owns a transport gate - a smoothed `MixDSP` - open while
`PlaybackShared::playing` is set. The flag flips at once for `Player::is_playing()` while the output
ramps to zero, and the media clock advances by the length of that ramp. A processor's first block
adopts the transport state rather than fading into it.

`TrackFade::play` snaps only when its mix has settled, so a fade still in flight keeps its ramp.

One block is the whole budget, shared by every processor in the graph.
`tests/benches/rt_block_budget.rs` derives it from the host's block and rate, and measures the share
`PlayerNodeProcessor` takes.

The audio thread never logs. Discrete facts leave through `PlayerNotification`, rates and faults
through `PlaybackShared::metrics()` (`bridge::RtMetrics`); `RtSink` carries both into the per-track
read path.

**Memory ordering.** `playing` and `seek_epoch` decide whether the audio thread acts and carry
`SeqCst`; touched once per command or block, never per sample. The `fetch_add` in
`next_seek_epoch` **is** the publication - storing its result back would let two concurrent seeks
reinstate the older epoch. Everything else is `Relaxed`: `RtMetrics` counters are monotonic deltas,
and `PlaybackSnapshot`'s scalars are independent readouts that need not agree across a block.

## Seek Ownership

A seek is split in two, because beginning one publishes an event and wakes the decode worker - both
lock-taking, both forbidden on the device callback.

**Control thread begins.** `EngineImpl::begin_slot_seek` walks the slot's `SeekBindings` and calls
`kithara_audio::SeekBegin::begin` on each. A handle is bound in `send_slot_cmd` when its resource
crosses to the audio thread and released on `Unloaded`.

**Audio thread re-bases.** `PlayerCmd::Seek` carries only data; `PlayerResource::reset_for_seek`
calls the reader's lock-free `sync_seek`, which adopts that epoch and recycles stale chunks into the
trash outlet. Nothing here can block or fail, so there is no seek-failure signal. The command still
carries `seek_epoch`, so one minted before a newer seek is dropped rather than applied.

`PcmControl::seek` remains the one-call form for consumers off the audio thread. A reader whose
`seek_handle` is `None` cannot be seeked from the callback at all.

`PlayerResource::scratch_frames` is a per-channel **frame** count, and `write_len` / `write_pos` /
the `read` range share that scale. Sized once per resource off the audio thread and never re-zeroed,
so every mixing path must fill its whole window - an underrun zero-fills rather than returning short.

Loaded tracks live in `rt::TrackSlots`, a fixed `[Option<PlayerTrack>; MAX_TRACKS]`. A `TrackSlot`
stays valid while its track lives, and iteration is slot order, so cleanup and eviction are
deterministic. `insert` hands a rejected newcomer back, since a `PlayerTrack` must not be freed on
the audio thread.

When the arena's last track ends at *natural* EOF the processor keeps it resident but inert, so a
later in-range seek can revive it; tracks finished via stop or a faded-out crossfade are discarded.

**A published seek outranks a natural end.** `seek_seconds` publishes the next epoch on
`PlaybackShared` *before* it sends the matching `PlayerCmd::Seek`, so a render block can drain the
feeder in the window between. Ending the track there would be reporting a position the user has
already left: the queue takes `ItemDidPlayToEnd` as the authoritative end of track and auto-advances,
flipping the current item out from under the seek the processor is about to apply. So each track
carries the slot epoch it has been re-based onto, `RtSink` carries the published one, and
`handle_natural_end` refuses to finalize while they differ. The hold costs blocks of silence, never
the signal: `apply_seek` re-bases *every* loaded track onto the applied epoch — including the ones
the seek does not move — and a track planted later starts at the epoch already published, so no track
can be born behind. `Failed` is not held: a broken source stays broken across a seek.

The refusal covers only ends not yet minted. An end the track finalized *honestly* — epochs equal
at mint time — sits in the slot's notification ring until the control thread drains it, and a seek
published in that window revives the track (`apply_seek` re-bases a `Finished`-at-EOF track back to
life) while the stale report is still queued. So every `PlaybackStopped` carries the epoch its track
sat at when minted, and `dispatch_notification` drops an `Eof` whose epoch is no longer the
published one (`slot_playback().seek_epoch`). The comparison is `!=`, not `<`: epochs wrap, and
withdrawal legally steps the published value back. `Stop` and `Failed` are never fenced, and a slot
with no playback state delivers as-is.

Publishing is a promise that a re-base is coming, and the send can fail — a full slot command ring
answers `PlayError::SlotChannelFull`. A promise nobody carries would hold the natural end forever, so
`seek_seconds` withdraws the epoch on a send error (`PlaybackShared::withdraw_seek_epoch`).
Withdrawal is a compare-exchange against the published value: a newer seek having published in the
meantime makes it a no-op, because that seek carries its own command and rolling back over it would
strand *it* instead.

The shared decode worker's produce core lives in `kithara-audio`
(`runtime/scheduler.rs::produce_tick_rt`) and carries the same attribute; off-core work
(pooled-buffer free, event flush, parking, symphonia allocation) belongs to the scheduler shell.
Cross-thread wakes reached on the core are *armed* lock-free (`kithara_stream::DeferredWake::arm`)
and delivered by the shell (`flush`), never on the forbid path.

Verification is gated by `--cfg rtsan`, so stable/production builds are byte-identical. Lanes:
`just test rtsan` (mock decoder), `rtsan-file` / `rtsan-hls` (real decoder, `suite_stress`), and
`rtsan-async` (`--features no-block`). A whole test body counts
as a nonblocking region only in the `no-block` lane, where `.config/rtsan/async-suppressions.txt`
narrows the check to genuine waits; the decoder lanes check the product's forbid regions alone and
their harness allocates freely. `deep:nightly` runs all four through `just ci run deep`, where a
violation fails the job. `permit()` and the RT attribute macros live in `kithara-test-utils` /
`kithara-test-macros`; there is no separate rtsan crate.

## Session Hosting

Platform-asymmetric by necessity. Native (`session/native.rs`): a dedicated engine worker thread
drains an `mpsc::Receiver<CmdMsg>` and replies per command; ring buffers live in session state /
engine slots, not in the command host. Web (`session/web/{bridge,client}.rs`): `AudioContext` lives
on the browser main thread and Worker-side clients proxy commands over an `mpsc` bridge. The
cross-platform core (`session/{state,dispatch,protocol,graph}.rs`) carries zero `#[cfg]`; the
structural gates are the cfg lines around `mod native`, `mod web`, and their re-exports in
`session/mod.rs`.

`SessionDispatcher::consumer_wake_mode` is the session's required, object-safe
consumer capability. Real-time session implementations explicitly return
`RealtimeDeferred`, preserving the audio callback's no-syscall drain path;
off-RT sessions return `ImmediateOffRt`, and dispatcher wrappers must forward
their inner capability. Requiring the method keeps wrappers from silently
erasing an off-RT capability through a trait default. `ConfigPrep` copies the
capability through an internal, builder-skipped `ResourceConfig` field into
`AudioConfig`. There is no public resource setter and therefore no second
source of session wake policy.

### Session transport anchor

`TransportCommitState` is the only owner of when the session beat-to-frame
relation changes. Every applied transport commit creates one `SessionAnchor`
at that exact render boundary and stores it in `SessionTransportSnapshot`. A
stream restart removes the snapshot because the session frame axis restarted;
the first block on the new axis reanchors the preserved beat before publishing
another snapshot.

## Session Mixing

Session-input gain has two distinct owners. Each `EngineImpl` owns its *desired* input level
(`master_volume`, read by `start`). The session `SessionState` owns the *applied* graph gain (each
`PlayerState.master_volume` and its `VolumeNode` memo). The only transition between them is one
batch command, `Cmd::SetPlayerMasterVolumes`, which validates the whole vector - every level finite
and in `0.0..=1.0`, every player present, no player repeated, graph initialised for started players

- before mutating any stored volume or memo. Omitted players are unchanged; an invalid entry leaves
  the whole batch untouched. This is the single session gain path; there is no singular per-player
  gain command.

Session-input levels are linear **amplitudes**: `0.5` halves the player's amplitude, and the
sub-ceiling session output is the exact weighted sum `Σ levelᵢ · signalᵢ`. Firewheel's
`Volume::Linear` is a fader taper (it squares its argument), so `master_gain` converts a level to
that taper before it reaches the player's `VolumeNode`; feeding a level in directly would land
`0.5` at `0.25` amplitude. Pure stereo gain stages never use a pan node because a centered
equal-power pan attenuates both channels.
Slot/content volume and session ducking keep their own taper; they are separate controls.

`apply_mix` (`engine/mix.rs`) is a free function actuating the final levels of a set of players in
one batch. The session is taken from the first input; it validates levels, same-session membership
(`Arc::ptr_eq` on the dispatcher), and per-batch uniqueness, then takes every input engine's
`start_lock` in stable address order - so a batch cannot interleave with a concurrent `start`, and
two batches sharing players cannot deadlock. Only already-registered engines (those with a session
`PlayerId`) enter the dispatched subset; a never-started engine has its desired level stored so its
first `start` adopts it, without speculative registration. Desired levels are committed only after
the dispatch succeeds. Committing publishes no event (`PlayerEvent::VolumeChanged` stays the only
volume event actually published); the desired level is readable via `EngineImpl::master_volume`.

The crossfader is pure policy (`api/mix.rs::crossfader_gain`), not state: consumers fold
`trim * mute * crossfader_gain * group_master` into each member level before calling `apply_mix`.
`group_master` is folded per member, never stored as process-wide state, so one logical group
cannot change another group's master. `crossfader_gain` rejects a non-finite or out-of-range
position with `PlayError::MixPosition` and returns exact `0.0`/`1.0` at the endpoints.

The session graph carries exactly one peak limiter (`rt/limiter.rs`, wrapping
`kithara_audio::PeakLimiter`) at the final sum, after session ducking and before the graph output.
It is created once per session context in `create_session_output`; recreating the context (route
change, idle teardown) rebuilds it with a fresh envelope. Player start/stop only connects or
disconnects player master nodes and never creates a per-player limiter. The session constants
(ceiling `0.98`, release `50 ms`, stereo) are const-asserted valid, so limiter construction cannot
fail in practice and the processor holds the DSP directly.

## Session Mix Tap

`Cmd::QuerySampleRate` reports `Option<u32>` from Firewheel's current `stream_info`: `None` means
the session has not measured an output yet. `sample_rate_hint` remains input to stream creation and
restart, never an observed-rate reply. Consumers that require the device fact wait for `Some`;
playback policy may explicitly choose its configured rate while no stream exists.

`Cmd::EnableMixTap` hangs `rt/tap.rs`'s `TapNode` off the session limiter beside `graph_out`:
stereo in, zero outputs, `ProcessStatus::ClearAllOutputs`. Firewheel's compiler sorts every node
topologically and keeps a sink with no outgoing edges in the schedule, so the extra
`limiter -> tap` edge is an addition to the terminal chain: the tap reads the limiter's output
buffer that `graph_out` interleaves into the device, and writes nothing anywhere.

The processor owns the `MixTapWriter` (`bridge/channels.rs`): a `ringbuf::HeapProd<f32>` carrying
the mix as interleaved stereo (LRLR) and an `Arc<AtomicU64>` drop counter. The ring's capacity is
the caller's to choose, and the node keeps it as handed over. Pushes are frame-aligned, because a
half frame lost to a full ring would swap L and R for the rest of the feed; an even capacity
therefore accounts for every sample. **The counter is in samples** - frames x 2 - monotonic and
`Relaxed`, which orders it against nothing in the ring: a delta locates its gap no more precisely
than the window drained around it, and that is the resolution a consumer may claim.

`SessionState.mix_tap` is the one owner of both states a tap has: `Requested` while it waits for a
session output to hang off - enabling before the first `start_player` is allowed, and
`create_session_output` installs it - and `Installed` once it carries a `NodeID`. A second
`EnableMixTap` while either state holds fails with `SessionError::MixTapActive`, so the live
consumer keeps its feed.

`DisableMixTap` and idle teardown release the producer, which the consumer observes as
`Observer::write_is_held() == false` and reads as end of feed. Removed nodes ride the returned
schedule back to the control thread, so the writer is freed there rather than on the audio thread.

A stream restart keeps the tap running: Firewheel constructs a processor once per node, so
`stop_stream` / `start_stream` reuse the one holding the writer. **A restart that lands on a
different device rate ends the feed instead**, because the ring carries bare samples and a consumer
holding it would read the new rate as the old one. `new_stream` compares rates and releases the
producer on the control thread; `state.mix_tap` still reads `Installed`, so the consumer's path
back on air is `DisableMixTap` and a fresh `EnableMixTap`.

The consumer owns the end of the tap's life: dropping its ring half leaves the node feeding a ring
nobody reads, and the node cannot resign on its own (releasing the producer there would free memory
on the audio thread). A consumer that stops reading sends `DisableMixTap`.

## Route Changes

`PlayerNodeProcessor::new_stream` is the host-rate bridge: it updates the shared sample rate and,
only when the numeric rate actually changed, propagates it to every loaded resource. Those
resources then enter the existing decoder recreate path with `RecreateCause::RouteChange`, which
preserves playback position and gapless state. Equal-rate notifications refresh host state and do
not recreate. The recreated decoder receives the current host rate through `DecoderConfig.resampler`:
Apple fused builds use the codec-embedded placement, other builds the standalone decoder-adapter
placement with the selected backend.

`ResourceConfig.decoder` (an `AudioDecoderConfig<B>`) is the only resource-level owner of decoder
construction settings - backend selection, gapless mode, and resampling. Its `resampler` field
threads the backend handle and tunables into `AudioConfig` and then `DecoderConfig.resampler`.
`ResourceConfig` is generic over that backend, defaulting to `PlaybackResamplerBackend` (Rubato,
else Glide, else none - by feature); leaving the field unset resolves to `B::default()` in
`kithara-audio`, which owns that fallback. Callers wanting a platform backend such as Apple
AudioConverter inject it through `ResourceConfig.decoder`, never through separate resource-level
resampler fields.

## Feature Flags

| Feature | Default | Effect |
| --- | --- | --- |
| `backend-cpal` | yes | CPAL output via `firewheel/cpal` |
| `backend-web-audio` | no | WebAudio backend (wasm32); implies `symphonia` |
| `wasm-bindgen` | no | WASM bindings via `firewheel/wasm-bindgen` |
| `symphonia` | yes | Software decode forwarded to `kithara-audio` / `kithara-decode` |
| `fdk-aac` | no | FDK-AAC decode forwarding |
| `webcodecs` | no | WebCodecs decode forwarding |
| `apple` | no | Apple AudioToolbox decode; does not imply Rubato |
| `apple-fused-src` | no | Apple fused decode+SRC via decoder-embedded resampler placement |
| `resample-rubato` | yes | Default fixed-ratio Rubato backend |
| `resample-glide` | no | Glide resampler backend for explicit config selection |
| `analysis-beat` | yes | Beat-analysis pass forwarding; absent from Apple FFI device sets |
| `analysis-waveform` | yes | RealFFT waveform analyzer forwarding |
| `client-reqwest` | yes | Forward the reqwest HTTP backend to network-reaching deps |
| `client-wreq` | no | Forward the wreq HTTP backend to network-reaching deps |
| `tls-rustls` | yes | Forward rustls TLS selection to network-reaching deps |
| `tls-native` | no | Forward native TLS selection to network-reaching deps |
| `probe` | no | USDT runtime tracing opt-in (forwards nothing on its own) |
| `mock` | no | Exposes the `mock` module (`EqualizerMock`) |

`src/guard.rs` hard-fails misconfigured builds: wasm32 requires `backend-web-audio`,
`wasm-bindgen`, and a resampler backend (`resample-rubato` or `resample-glide`); non-wasm
requires `backend-cpal`. The resampler is a build requirement on wasm because the web-audio
context runs at the browser's rate and nothing downstream bridges to it: without a backend
`PlaybackResamplerBackend` is `NoResamplerBackend` and every off-rate track fails to open.

File and HLS pipelines are unconditional: `kithara-play` always links `kithara-file`,
`kithara-hls`, `kithara-abr`, `kithara-assets`, `kithara-net`.

## Current Item

`PlayerEvent::CurrentItemChanged` means the *identity* of the current item changed - not merely
that playback (re)started. A bare `play()` resuming the already-current item must **not** emit it:
consumers (the queue, which re-publishes `QueueEvent::CurrentTrackChanged`, and FFI observers)
treat it as a track switch and do real work, e.g. re-analysing the waveform.

`Playlist` owns both the current index and the announce-dedup state. Its
`last_announced: Option<usize>` starts as `None`; `mark_announced` records an index and reports
whether it changed. `ItemQueue::announce_current_item` is the sole event publisher and emits only
when that report is true, so first activation announces but a resume does not. Genuine track moves
(`commit_next`, `advance_to_next_item`, the handover finaliser, and the select/jump path) route
through that publisher. Playlist mutations that can change identity under a reused index (`clear`,
`remove_at`, and replacement of the announced index) reset the dedup state to `None`, so the next
`play()` re-announces. `play()` announces only when the item actually loaded - an empty slot means
the load is still in flight, and announcing would make the arriving resource take the
reselecting-current path and never be enqueued.

## Invariants

- `SlotId` is valid only between `allocate_slot()` and `release_slot()`.
- At most `EngineImpl::max_slots()` slots allocated at once (`PlayerConfig.max_slots`, default 4).
- `PlayerImpl::slot()` is `None` until a slot is allocated (phases `Idle` and `Stopped`-without-
  slot); `send_to_slot` then fails with `PlayError::NoActiveSlot`.
- Audio-thread `process()` is allocation-, free-, and lock-free.
- `duration_seconds()` returns `None` while duration is unknown; the shared atomic's `0.0`
  conflates "unknown" with "empty track", so callers must not read it directly.

## Testing And Integration

The offline render backend for deterministic engine/player tests lives in
`kithara-integration-tests::offline` (`tests/src/offline/`), not here. Enable `mock` for the
`Equalizer` unimock helper. `session::testing::test_session()` provides an in-process dispatcher
for unit tests.

Public failures propagate as `Result<T, PlayError>`; no `unwrap()` / `expect()` in production code.
