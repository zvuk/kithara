# kithara-play - Context

The README is the overview and the module tree is the map; repo-wide rules stay
in `AGENTS.md`. This file carries only what neither the sources nor a named test
can state for itself.

## Planes & Ownership

`player`, `engine`, and the lower session protocol are deliberate orchestration
planes - their entry files bind API state, RT controls, and session commands -
so `.config/arch/thresholds.toml` raises `module_fan_out` to 9 for this crate.
Do not add re-export hops solely to lower that count.

## Buffer Pool Ownership
The application composition root declares one closed schema with
`kithara_bufpool::pool_schema!` and builds one `PoolRegion<S>`. Player,
resource, worker, Warp, and session registration propagate that one schema
instead of separate byte and sample pools; `HasPool<K>` makes an unregistered
pool a compile error at the root. Budget semantics belong to
`crates/kithara-bufpool/CONTEXT.md`. The master EQ is the one deliberate
degraded mode: a failed control-thread scratch allocation leaves
`MasterEqProcessor` a transparent bypass with a `warn` - a degraded mode, not a
second allocation path.

## Domain Policies
`policy` is the orchestration-level owner of domain matching - not
`kithara-assets`, `kithara-platform`, or `kithara-drm`. Host matching is
case-insensitive and first-match-wins over ordered rules; application-declared
query keys stay case-sensitive. `QueryIdentityLayout` combines an existing
`AssetSource::Remote` discriminator with the query identity rather than replacing
it. The DRM `KeyProcessorRegistry` is opaque, so a caller that also needs
playlist or segment headers keeps the same immutable `Arc<DomainKeyPolicy>` and
asks it via `resource_headers` - never a second policy source of truth.
`tests/asset_policy.rs` and `tests/drm_policy.rs` pin which query pairs fragment
the cache, wildcard and ordering precedence, and header merge order.

## Per-track producer chain
`PlayWorker::open` is the sole supported production composition path in this
workspace. It asks
`kithara-audio` for `PreparedAudio` plus a still-concrete `AudioSource`, wraps the
reader as resident identity `Warp<Audio<Stream<T>>>`, creates its synchronous
`WarpRenderer`, builds the post-Warp track effects, and wraps the source in
`WarpSource`.
`DecoderNode` then owns final output admission, preload completion, decoded
frontier publication, terminal event publication, and per-track load
measurement before the task is registered on the shared worker.

`PlayWorker` derives a dedicated dispatcher from `kithara-worker` and supplies
the play-owned node and observer. `kithara-audio` exposes only the prepared
source and wake contracts. No workspace production code can construct a second
playback scheduling path.

There is one chain and one final output path:

`decoded source -> WarpRenderer -> custom effects -> final output ring`

`DecoderNode` admits each produced chunk or terminal marker directly into the
bounded final ring. A full ring returns `TickResult::Backpressured` before
another source step; final playback output is never parked in the generic
`Outlet` overflow slot.

The Warp wrapper is resident even when synchronization is off, so passthrough
does not select a second implementation. R7 does not yet evaluate `WarpMap`,
advance a runtime map cursor, apply non-identity alignment, or call
`SyncGroup::acknowledge`; final-ring admission is rendered readiness, not proof
that a device callback presented those frames.

`WarpRenderer::prepare` services format/backend changes and retired engine
state between checked ticks. `AudioEffect::service_deferred` then prepares
post-Warp effects. With an elastic backend, live input is split into the
configured render quantum; controls are sampled
for each quantum and `WarpSource` advances only its matching source span. The
identity/no-backend renderer preserves whole-chunk passthrough. The elastic
quantum bounds live control response and source processing even when the
decoder yields a larger chunk. It does not bound terminal output: at true source
EOF, `WarpSource` drains the Warp tail according to the backend tail contract
before `EffectDrain` flushes the effects themselves, preserving multi-pull tails before
`DecoderNode` publishes the decode-epoch-tagged diagnostic
`AudioEvent::EndOfStream`. That raw event is not a user-visible playback-end
signal: `PlayerEvent::ItemDidPlayToEnd` is the sole path to FFI `DidReachEnd`
after the player fences the terminal marker against the live seek epoch. Seek
or source-discontinuity revision changes reset the chain before new-generation
decoded audio is admitted.

`sanitize_sample` from `kithara-signal` runs at the input of play-owned DSP
stages that accept untrusted samples. `IsolatorEq` flushes denormal IIR state to
exact zero only after both input and output fall below `f32::MIN_POSITIVE`;
finite unity/bypass samples remain bit-exact.

## Tempo & Key-Lock

`kithara_warp::StretchControls` (one `Arc` per deck, in `PlayerConfig.warp`)
owns the requested playback target. `prepare_config` carries the complete
`WarpConfig` into `ResourceConfig`; `Resource` puts the same config in
`TrackConfig::warp`, and resident `Warp<S>` keeps it beside the reader while
its `kithara-warp::WarpRenderer` reads the controls at the configured render
quantum in frames. It always carries `speed` + `region_plan`; with a native backend
compiled by `kithara-play` (`stretch-signalsmith` / `stretch-bungee`) it also
carries `keylock` and `backend`. `Queue` delegates the target to the player;
key-lock and backend are set directly on the same handle.

Backend kind is configuration, not a live musical control. A change requested
while the resident engine still holds source is applied at the next
reset/discontinuity, spec change, or failed-engine rebuild; it never discards a
live backend tail.

The canonical controls are seeded before a track is loaded; the native
`WarpRenderer` does not cache a second requested target. A `PlayerTrack` advances
its media clock from the reader-position delta represented by frames actually
consumed from its feeder scratch. Read-ahead and an un-applied rate target
therefore cannot move near-end or handover triggers ahead of presented media.
The leading track publishes the deck's effective rate through `PlaybackShared`.
`Player::rate()` and `PlayerEvent::RateChanged` expose only that effective
value: fixed/no-backend resources report `1.0`, while a paused deck or a deck
without a leading track reports `0.0`. Target controls and the effective
observation therefore cannot silently diverge in the public surface.
With a backend compiled in, `WarpRenderer` runs at `ratio = 1/speed`, and:

- **key-lock off** (the constructed default): `pitch = speed` - speed shifts pitch, vinyl-style.
- **key-lock on**: `pitch = 1.0` - speed preserves pitch.

Before the first active stretch, exact speed 1.0 with no region plan can use
zero-copy passthrough. Once an active stretch has been created, later exact 1.0
stays in the resident engine and does not return to passthrough or reset/drain
its buffered history. Engine reset/rebuild boundaries are seek, source
discontinuity, and spec/backend lifecycle; terminal EOF tail drain is separate.
Without a backend - including every wasm build - the same Warp slot remains in
the producer chain as an exact identity renderer; no speed DSP is inserted and
decoded-audio output stays pinned to 1.0. With a native backend, speed and
key-lock controls are read for each live render quantum. A requested backend
kind becomes current only at a reset, spec-change, or failed-engine rebuild
boundary, so changing it never discards buffered source history.

Fixed-ratio sample-rate conversion is a separate stage: Apple fused builds use the codec-embedded
placement, other builds the standalone decode-adapter resampler.

`RegionPlan` lives in `kithara-warp`. Its sorted, non-overlapping segments are
expressed in source-frame space; the renderer splits chunks at region
boundaries and combines `1/speed` with each segment's finite positive ratio.
A region boundary preserves backend history and is not a source
discontinuity. Stretch changes output frame count, not `AudioSpec.sample_rate`,
and carries source-time metadata forward.

## Engine Load
`PlayerCore` creates one address-stable `EngineLoad` meter and passes the same
`Arc` through `TrackConfig` into `DecoderNode`. Neither `AudioConfig` nor
`PreparedAudioLane` carries it, so there is no second route for load to reach a
node.

## Live Equalizer Layout
`PlayerImpl::set_eq_layout` replaces a running player's master EQ, and **the
session graph is the actuator**: it builds the replacement on the control thread,
reconnects every slot through it, removes the old EQ, and submits one graph
update. The audio thread never allocates, locks, or rebuilds filters for a layout
change. `SharedEq` is the control-plane gain mirror shared by session and slot
handles - **no audio processor reads it**; the DSP takes gains from the session's
node event queue. Replacement swaps the whole band array behind the `ArcSwap`
every handle clone points at (`a_handle_clone_sees_the_replacement_band_array`).

## Events
One `kithara_events::EventBus` per player: `player.subscribe()` and
`engine.subscribe()` return receivers on the *same* bus. `SessionDuckingMode` is
owned here and maps `Off` / `Soft` / `Hard` to session-output gains `1.0` / `0.4`
/ `0.2`.

**A slot is a processor holding an arena of items, not one item.**
`process_notifications` drains every active slot, so a bare start or stop says
nothing about what the listener is hearing, and only this crate can say. Hence
`PlaybackStarted`, `ItemDidPlayToEnd`, and `ItemDidFail` carry an `ItemRole`
holding the `TrackRef` *inside* the role: a consumer cannot reach the identity
without first declaring which item it has. `Leading` alone drives auto-advance;
`Outgoing` is the half promoted over inside the current slot, which slot identity
alone would misread as leading; `Background` is a slot the phase no longer holds,
reaching its own end seconds into the current item. The five cases are pinned in
`player/flow/notify.rs`. Within `TrackRef`, **only `id` answers "which entry"**:
`slot` is an arena and `src` names a rendered resource two entries may share.

## Queue Auto-Advance
`arm_next` is idempotent per index and unloads a differently-armed one;
`commit_next` is for `cf>0` only, because the audio thread stitches `cf=0`
internally. `PlayerConfig::auto_advance_enabled` (default `true`) applies a
built-in linear policy. **`kithara-queue::Queue` disables that policy and reacts
to `PlayerEvent::HandoverRequested` through `select_item_with_crossfade`; it
never calls `arm_next` or `commit_next`.** `select_item_with_crossfade` fails
`PlayError::ItemConsumed` *before any bookkeeping* when the target index is
neither armed, nor the announced current item, nor still holding a resource - the
UI must not drift from the audio
(`select_item_on_consumed_slot_errors_without_bookkeeping`).

## Engine Lifecycle
`EngineImpl::start` is **atomic single-start**: an internal `start_lock`
serializes the `running` check-then-act, and `running` flips only after
`session.start_player` fully succeeds. `ensure_engine_started` treats
`EngineAlreadyRunning` as success, so a concurrent start is idempotent rather
than a session desync. `stop_player` releases the output device once no player is
started; the next `start_player` builds a fresh context.

**Drop order is load-bearing and invisible in any one file.** `PlayerImpl`
declares `phase` before `core`, and `PlayerCore` declares `items` before
`engine`, so undelivered resources (which hold worker references) release before
the engine and resource registrations. `worker` is declared after those owners,
so the final `PlayWorker` reference cannot shut its thread down while a track
still holds a lease; likewise the dispatcher precedes the base-worker clone, and
each lease's task handle precedes its worker clone.

## Cancel Hierarchy
`docs/guides/cancel-policy.md` owns the typed propagate-down tree, the
`CancelScope` seam, and the `cancel_root_sites` allowlist (kithara-play is not on
it). Consumer crates mint a root and pass a child in `PlayerConfig.cancel`, while
`None` makes a fresh root; the same token goes to `EngineConfig.cancel`, and the
playback dispatcher has its own explicit lifetime in `PlayWorkerConfig.cancel`.
`prepare_config` gives every track a `.child()` of the player token
(`prepare_config_per_track_cancel_is_child_of_player_master`), and
`PlayerImpl::drop` cancels its own subtree, never the potentially foreign parent
it was handed.

`Resource` holds a `CancelGuard` around the per-track token, declared before
`inner`, so a mid-session unload tears down the whole track subtree (stream *and*
`Audio`) before the reader drops. The `From<Resource>` reader unwrap disarms that
guard, because the live reader then outlives the wrapper and rides the
`kithara-analysis` run-scope cancel instead.

## Real-Time Audio Thread
`docs/guides/performance/realtime.md` owns the general callback rules. The
play-owned `#[kithara::rtsan_forbid_blocking]` sites are
`PlayerNodeProcessor::process`, `MasterEqProcessor::process`, and
`DecoderNode::tick`; the session limiter and mix tap processors belong to
`kithara-host`. Specific to this crate:

- **Nothing is freed on the audio thread.** Finished or evicted tracks go to the
  bounded trash ring from `bridge::slot_channels`, drained by the main thread in
  `process_notifications`. A full command ring surfaces as
  `PlayError::SlotChannelFull`, never as a block.
- Render scratch is sized in `new_stream`, off the audio thread, and
  `render_audio` clamps its block to it, so a host exceeding its declared
  `max_block_frames` loses that block's tail rather than allocating. One block is
  the whole budget, shared by every processor in the graph;
  `tests/benches/rt_block_budget.rs` derives it and measures this node's share.
- **Pause and resume are ramps.** The flag flips at once for
  `Player::is_playing()` while output ramps to zero, and the media clock advances
  by the ramp's length. A processor's first block adopts the transport state
  rather than fading into it, and `TrackFade::play` snaps only once its mix has
  settled, so a fade in flight keeps its ramp.
- Facts leave the audio thread through `PlayerNotification`, rates and faults
  through `PlaybackShared::metrics()`.
- **Memory ordering.** Only `playing` and `seek_epoch` decide whether the audio
  thread acts, and only they carry `SeqCst`. Everything else is `Relaxed`,
  because `RtMetrics` counters are monotonic deltas and `PlaybackSnapshot`'s
  scalars are independent readouts that need not agree across a block.

Verification is gated by `--cfg rtsan`, so stable and production builds are
byte-identical. A whole test body counts as a nonblocking region only in the
`no-block` lane, where `.config/rtsan/async-suppressions.txt` narrows the check
to genuine waits; the decoder lanes check the product's forbid regions alone.

## Seek Ownership
**A seek is split in two**, because beginning one publishes an event and wakes
the playback worker - both lock-taking, both forbidden on the device callback.
`EngineImpl::begin_slot_seek` begins it on the control thread;
`PlayerResource::reset_for_seek` re-bases on the audio thread through the
reader's lock-free `sync_seek`. **The audio half can neither block nor fail, so
there is no seek-failure signal.** A seek handle is bound in `send_slot_cmd` once
its resource is accepted by the audio-thread channel, and released by exact item
*and* resource identity when that resource returns through the trash channel, so
replacing one item or reusing a URL cannot detach a different resident track.

**A published seek outranks a natural end.** `seek_seconds` publishes the next
epoch on `PlaybackShared` *before* sending the matching command, so a render
block can drain the feeder in that window; ending the track there would hand the
queue an `ItemDidPlayToEnd` it answers by auto-advancing out from under the
accepted seek. Four sites carry the fence, and their own doc comments carry the
derivation:

- `handle_natural_end` holds while the track's epoch differs from the published
  one, costing blocks of silence, never signal.
- `apply_seek` re-bases *every* loaded track - including ones the seek does not
  move, and a `Finished`-at-EOF track back to life - and a track planted later
  starts at the published epoch, so none can be born behind.
- `dispatch_notification` drops a stale `Eof`. The comparison is `!=`, not `<`:
  epochs wrap, and withdrawal legally steps the published value back. `Stop` and
  `Failed` are never fenced - a broken source stays broken across a seek.
- `PlaybackShared::withdraw_seek_epoch` is a compare-exchange, so a failed send
  cannot strand its own seek and a newer seek makes withdrawal a no-op
  (`withdrawing_an_overtaken_epoch_leaves_the_newer_seek_published`).

`PlayerResource::scratch_frames` is a per-channel **frame** count shared by
`write_len` / `write_pos` / the `read` range, sized once off the audio thread and
never re-zeroed, so **every mixing path must fill its whole window** - an
underrun zero-fills rather than returning short.

`rt::TrackSlots::insert` hands a rejected newcomer back rather than freeing a
`PlayerTrack` on the audio thread. When the arena's last track ends at *natural* EOF the processor
keeps it resident but inert so a later in-range seek can revive it; tracks
finished by stop or a faded-out crossfade are discarded. Off-core worker work
(pooled-buffer free, event flush, parking, symphonia allocation) belongs to the
scheduler shell, and cross-thread wakes reached on the core are *armed* lock-free
and delivered by the shell, never on the forbid path.

## Session Actuator
`kithara-play` owns the lower object-safe `SessionDispatcher` protocol used by
one Player. `kithara-host` owns every concrete native/web session, the existing
`kithara-engine` thread, the Firewheel graph, and the root synchronization
group. A Player is constructed as an unbound instance and receives an opaque,
one-shot `SessionBinding` only through `Host::insert`; decorators may only
delegate the binding to their resident Player. Native insertion transfers the
whole resident player. Wasm insertion transfers its `GroupState` and current
desired level to the main-thread Host, which becomes their canonical
owner, while the Worker Host retains the runtime and JS-bound resources. There
is no standalone concrete production session constructor in this crate.

`kithara-warp` owns musical coordinates plus the `BeatGrid`, `WarpMap`, and
`SyncGroup` protocols; `kithara-play` owns one Player instance and
`kithara-host` owns the live session that composes Players. `TrackBinding` captures an
owner-published `BeatGridSnapshot`; it neither creates a grid identity nor
converts analysis facts into a second representation. R7 session state may
transact pure group operations, but the producer chain does not yet consume a
`WarpMap` or acknowledge rendered/presented application.

`SessionDispatcher::consumer_wake_mode` is the session's required, object-safe
consumer capability. Real-time session implementations explicitly return
`RealtimeDeferred`, which lets the audio callback arm a coalesced atomic worker
level without a syscall;
off-RT sessions return `ImmediateOffRt`, and dispatcher wrappers must forward
their inner capability. Requiring the method keeps wrappers from silently
erasing an off-RT capability through a trait default. `ConfigPrep` copies the
capability through an internal, builder-skipped `ResourceConfig` field into
`AudioConfig`; an unbound direct `Resource` resolves the absent session
capability to `ImmediateOffRt`, giving it immediate worker wakes and inline
reader-event delivery. There is no public resource setter and therefore no
second source of session wake policy. A real-time read arms the coalesced worker
level; later reads coalesce until the worker consumes it. While a final ring is
backpressured, the dispatcher polls that level at the playback worker's bounded
interval; its ordinary timeout remains only a liveness backstop.

### Host transport anchor

The Host's `TransportCommitState` is the only owner of when the session beat-to-frame
relation changes. Every applied transport commit creates one `SessionAnchor`
at that exact render boundary and stores it in `SessionTransportSnapshot`. A
stream restart removes the snapshot because the session frame axis restarted;
the first block on the new axis reanchors the preserved beat before publishing
another snapshot.

## Session Mixing
**Session-input gain has two distinct owners.** Each `EngineImpl` owns its
*desired* input level (`master_volume`); the session `SessionState` owns the
*applied* graph gain. Production crosses that boundary only through
`Host::apply_mix`, which validates the whole vector before mutating anything:
omitted players are unchanged, one invalid entry voids the whole batch, and
desired levels commit only after dispatch succeeds. There is no singular
per-player gain command. `engine/mix.rs` pins the validation
(`apply_rejects_duplicate_player_without_mutation` and siblings).

**Session-input levels are linear amplitudes**, so sub-ceiling session output is
the exact weighted sum of level times signal. Firewheel's `Volume::Linear` is a
fader taper - it squares its argument - so `master_gain` converts a level to that
taper before the `VolumeNode`; feeding a level in directly would land `0.5` at
`0.25` amplitude. Pure stereo gain stages never use a pan node, because a
centered equal-power pan attenuates both channels. Slot/content volume and
session ducking keep their own taper.

`Cmd::SetPlayerMasterVolumes` and the matching session-handle method compile only
for tests or the `probe` feature, keeping deterministic offline render tests on
the lower `SessionDispatcher`; **it is not a second production path.** The
crossfader is likewise pure Host policy, **not state**: consumers fold trim,
mute, crossfader gain, and group master into each member level before calling
`Host::apply_mix`, and group master is folded per member rather than stored
process-wide, so one logical group cannot change another's master.

## Session Mix Tap
The tap is a Host-owned zero-output sink on the session limiter, reading the
buffer the device receives. Three facts bind a consumer:

- **The drop counter is in samples**, monotonic and `Relaxed` - it orders against
  nothing in the ring, so a delta locates its gap no more precisely than the
  window drained around it.
- A stream restart keeps the tap running, but **a restart landing on a different
  device rate ends the feed**, because the ring carries bare samples and a
  consumer holding it would read the new rate as the old one; the way back on air
  is `DisableMixTap` then a fresh `EnableMixTap`.
- The node cannot resign on its own, because releasing the producer there would
  free memory on the audio thread, so a consumer that stops reading sends
  `DisableMixTap`. End of feed is `Observer::write_is_held() == false`.

`Cmd::QuerySampleRate` reports `None` until the session has measured an output;
`sample_rate_hint` is stream-creation input, never an observed rate.

## Route Changes
`PlayerNodeProcessor::new_stream` is the host-rate bridge: it updates the shared
sample rate and, **only when the numeric rate actually changed**, propagates it
to every loaded resource, which then enter the existing decoder recreate path
with `RecreateCause::RouteChange`, preserving position and gapless state.
Equal-rate notifications refresh host state and do not recreate.

`ResourceConfig.decoder` is the only resource-level owner of decoder construction
settings - backend selection, gapless mode, resampling. An unset resampler field
resolves to `B::default()` in `kithara-audio`, which owns that fallback; a
platform backend such as Apple AudioConverter is injected through
`ResourceConfig.decoder`, never through separate resampler fields.

## Feature Flags
`Cargo.toml` owns the list and defaults. `src/guard.rs` hard-fails misconfigured
builds - wasm32 without `resample-rubato` or `resample-glide`, non-wasm without
`stretch-signalsmith` or `stretch-bungee`. Decode, resampler, stretch,
HTTP-client, and TLS features forward to a backend in a lower crate; none add
behaviour here. File and HLS pipelines are unconditional:
this crate always links `kithara-file`, `kithara-hls`, `kithara-abr`,
`kithara-assets`, `kithara-net`.

## Current Item
`PlayerEvent::CurrentItemChanged` means the *identity* of the current item
changed - **not merely that playback (re)started.** A bare `play()` resuming the
already-current item must not emit it: consumers treat it as a track switch and
do real work such as re-analysing the waveform. `Playlist` owns the current index
and the announce-dedup state, and `ItemQueue::announce_current_item` is the sole
publisher (`announce_deduplicates_current_item_event`); a mutation that can
change identity under a reused index resets the dedup state so the next `play()`
re-announces. `play()` announces only when the item actually loaded, since an
empty slot means the load is still in flight and announcing would send the
arriving resource down the reselecting-current path, never to be enqueued.

## Invariants
- Audio-thread `process()` is allocation-, free-, and lock-free.
- `duration_seconds()` returns `None` while duration is unknown; the shared
  atomic's `0.0` conflates "unknown" with "empty track", so callers must not read
  it directly.

## Testing And Integration
The offline render backend for deterministic engine and player tests lives in
`kithara-integration-tests::offline`, not here.
