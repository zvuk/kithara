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

Three Firewheel processors run on the audio thread and carry
`#[kithara::rtsan_forbid_blocking]`: `PlayerNodeProcessor::process` (`rt/processor.rs`),
`MasterEqProcessor::process` (`rt/eq.rs`), `LimiterProcessor::process` (`rt/limiter.rs`). They stay
allocation-, free-, and lock-free: render scratch is sized in `new_stream`
(`RenderPass::resize`), which Firewheel calls on the main thread, and finished or evicted tracks go
to the bounded trash ring from `bridge::slot_channels` instead of being freed on the audio thread -
the main thread drains it in `PlayerImpl::process_notifications`. A full command ring surfaces as
`PlayError::SlotChannelFull`, never as a block.

`render_audio` clamps its block to the scratch it holds rather than growing a pooled buffer, so a
host that exceeds the `max_block_frames` it declared loses the tail of that block instead of
allocating in the callback.

Pause and resume are ramps, not switches. `RenderPass` owns a transport gate - a `MixDSP` on a 5 ms
smoother - that mixes the summed track bus into the node output: open while `PlaybackShared::playing`
is set, closed while it is clear. A pause keeps the tracks rendering until the gate settles at
silence, so the flag flips at once for `Player::is_playing()` while the waveform reaches zero over
the ramp (~35 ms), and the media clock advances by that much. The first block a processor renders
adopts the transport state instead of fading into it - a ramp needs a previous frame to step from -
and a settled open gate copies the bus through unchanged.

`TrackFade::play` snaps only when its mix has settled: a start with no crossfade stays sample-exact,
while a fade still in flight keeps its ramp instead of jumping to full level - the path a seek during
a fade-in takes. `TrackFade::update_duration` re-arms the smoother at the state's end point, so an
unchanged duration - `PlayerImpl::play` re-sends the crossfade duration on every call - only follows
the sample rate.

One block is the whole budget: 128 frames at 48 kHz gives the device callback **2.667 ms**, shared
by the three processors and whatever the host adds. `tests/benches/rt_block_budget.rs`
(`cargo bench -p kithara-integration-tests --bench rt_block_budget`) times the body of
`process()` - `drain_commands`, `cleanup_finished_tracks`, `render_audio` - over a 1-, 2-, and
4-track arena and reports `PlayerNodeProcessor`'s share of it as p50 / p99 / max, because a mean
averages away the one block that overruns. Sources are ready-in-memory mocks, so the figure is the
callback's own mix cost: decode belongs to the worker, and the master EQ and limiter are separate
nodes. The bench asserts a non-silent block and an unmoved underrun counter, so a number can only
come from a real mix.

The audio thread never logs. Conditions the control thread needs to know about leave through one of
two lock-free channels: discrete facts through `PlayerNotification`, rates and faults through
`PlaybackShared::metrics()` (`bridge::RtMetrics`) - decode errors, underruns, audible tracks evicted
at capacity, and trash-ring overflows. `RtSink` carries both into the per-track read path.

**Memory ordering.** `PlaybackShared` splits by what a reader has to conclude from the value.
`playing` and `seek_epoch` decide whether the audio thread acts, so they carry `SeqCst`: both are
touched once per command or once per block, never per sample, so one barrier there is free next to
the mix, and a single total order removes the question of which pairs would have needed
acquire/release. The seek epoch is minted by the `fetch_add` in
`PlaybackShared::next_seek_epoch` - that RMW is the publication, and nothing may store its result
back, or two concurrent seeks would reinstate the older epoch. Everything else is `Relaxed`: the
`RtMetrics` counters are monotonic and read as deltas, and `PlaybackSnapshot`'s scalars are
independent progress readouts whose fields deliberately do not agree across a block boundary (see
its docstring).

## Seek Ownership

A seek is split in two, because declaring one publishes an event and wakes the decode worker - both
lock-taking, both forbidden on the device callback.

**Control thread declares.** `PlayerImpl::seek_seconds` calls `EngineImpl::declare_slot_seek`, which
walks the slot's `SeekBindings` and calls `kithara_audio::SeekDeclare::declare` on each. A track's
handle is bound in `send_slot_cmd` at the moment its resource crosses to the audio thread, and
released when the processor reports it `Unloaded` - the two halves of one object, like
`SlotControl` / `NodeInputs`, not a mirror of the arena.

**Audio thread re-bases.** `PlayerCmd::Seek` carries only data. `PlayerTrack::seek` drops the
feeder's scratch and moves the media clock, and `PlayerResource::reset_for_seek` calls the reader's
lock-free `sync_seek`, which adopts the declared epoch and recycles stale chunks into the trash
outlet. Nothing on this path can block or fail, so there is no seek-failure signal to report.

`PlayerCmd::Seek` still carries `seek_epoch`: a command minted before a newer one is dropped rather
than re-basing the clock to a superseded target.

`PcmControl::seek` keeps declaring and applying in one call for consumers that are not on an audio
thread (analysis, offline render). A reader whose `seek_handle` is `None` cannot be seeked from the
callback at all - those callers must reach it off-thread.

`PlayerResource`'s scratch is planar: `PlayerResource::scratch_frames` is a per-channel **frame**
count, and `write_len` / `write_pos` / the `read` range are on that same scale. Channel count does
not enter it. It is sized once per resource, off the audio thread, and never re-zeroed per block —
so every path that mixes must write its whole window, which is why an underrun zero-fills rather
than returning short.

Loaded tracks live in `rt::TrackSlots`, a fixed `[Option<PlayerTrack>; MAX_TRACKS]`. A track carries
its own `src`, so lookup is a linear scan over at most four entries rather than a side table keyed
by the same string, and a `TrackSlot` stays valid while its track lives - removing one never shifts
another. Iteration is slot order, so cleanup and eviction are deterministic even when every track
shares a state. `TrackSlots::insert` hands a rejected newcomer back instead of dropping it, since a
`PlayerTrack` must not be freed on the audio thread.

When the arena's last track ends at *natural* EOF the processor keeps it resident but inert, so a
later in-range seek can revive it; tracks finished via stop or a faded-out crossfade are discarded.

The shared decode worker's produce core lives in `kithara-audio`
(`runtime/scheduler.rs::produce_tick_rt`) and carries the same attribute; off-core work
(pooled-buffer free, event flush, parking, symphonia allocation) belongs to the scheduler shell.
Cross-thread wakes reached on the core are *armed* lock-free (`kithara_stream::DeferredWake::arm`)
and delivered by the shell (`flush`), never on the forbid path.

Verification is gated by `--cfg rtsan`, so stable/production builds are byte-identical. Lanes:
`just test rtsan` (mock decoder - the offline-harness smoke plus `kithara_play::rt_metrics`, which
drives `process()` through a decode error, an underrun, an eviction, and the on-core seek re-base),
`just test rtsan-file` / `rtsan-hls` (real decoder, `suite_stress` phase continuity), and
`just test rtsan-async` (`--features no-block`). The decoder lanes check the product's forbid
regions alone, and their harness allocates freely: a whole test body counts as a nonblocking region
only in the `no-block` lane, where `.config/rtsan/async-suppressions.txt` narrows the check to
genuine waits. GitLab `deep:nightly` runs all four through `just ci run deep`
(`xtask/src/ci/run.rs`) on macOS, where a violation fails the job;
`.github/workflows/rtsan.yml.disabled` mirrors the decoder lanes onto Actions and stays inert while
it carries that suffix. The toolchain comes from `KITHARA_NIGHTLY_TOOLCHAIN`, pinned for CI in
`.config/ci-pins.toml` and falling back to `nightly`. `permit()` and the RT attribute macros live
in `kithara-test-utils` / `kithara-test-macros`; there is no separate rtsan crate.

## Session Hosting

Platform-asymmetric by necessity. Native (`session/native.rs`): a dedicated engine worker thread
drains an `mpsc::Receiver<CmdMsg>` and replies per command; ring buffers live in session state /
engine slots, not in the command host. Web (`session/web/{bridge,client}.rs`): `AudioContext` lives
on the browser main thread and Worker-side clients proxy commands over an `mpsc` bridge. The
cross-platform core (`session/{state,dispatch,protocol,graph}.rs`) carries zero `#[cfg]`; the
structural gates are the cfg lines around `mod native`, `mod web`, and their re-exports in
`session/mod.rs`.

## Session Mixing

Session-input gain has two distinct owners. Each `EngineImpl` owns its *desired* input level
(`master_volume`, read by `start`). The session `SessionState` owns the *applied* graph gain (each
`PlayerState.master_volume` and its `VolumePanNode` memo). The only transition between them is one
batch command, `Cmd::SetPlayerMasterVolumes`, which validates the whole vector - every level finite
and in `0.0..=1.0`, every player present, no player repeated, graph initialised for started players

- before mutating any stored volume or memo. Omitted players are unchanged; an invalid entry leaves
  the whole batch untouched. This is the single session gain path; there is no singular per-player
  gain command.

Session-input levels are linear **amplitudes**: `0.5` halves the player's amplitude, and the
sub-ceiling session output is the exact weighted sum `Σ levelᵢ · signalᵢ`. Firewheel's
`Volume::Linear` is a fader taper (it squares its argument), so `master_gain` converts a level to
that taper before it reaches the player's `VolumePanNode`; feeding a level in directly would land
`0.5` at `0.25` amplitude and square the equal-power crossfader's `cos`/`sin` coefficients.
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
else Glide, else none - by feature). Callers wanting a platform backend such as Apple
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

`src/guard.rs` hard-fails misconfigured builds: wasm32 requires both `backend-web-audio` and
`wasm-bindgen`; non-wasm requires `backend-cpal`.

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
