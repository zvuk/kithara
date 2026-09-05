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
`PlayWorker::open` is the sole supported production composition path, and no
workspace production code can build a second playback scheduling path:
`PlayWorker` derives its dispatcher from `kithara-worker`, while `kithara-audio`
exposes only the prepared source and wake contracts. One chain, one final
output path:
`decoded source -> WarpRenderer -> custom effects -> final output ring`. The Warp
wrapper is resident even with synchronization off, so passthrough never selects a
second implementation. Sync is not yet wired end to end; final-ring admission is
rendered readiness, not proof that a device callback presented those frames.

At true source EOF `WarpSource` drains the Warp tail through the effect chain
before `EffectDrain` flushes the effects, preserving multi-pull tails.
`DecoderNode` then publishes the decode-epoch-tagged `AudioEvent::EndOfStream`.
**That raw event is not a user-visible end of playback.**
`PlayerEvent::ItemDidPlayToEnd` is the sole path to FFI `DidReachEnd`, and only
after the player fences the terminal marker against the live seek epoch (see Seek
Ownership). Seek and source-discontinuity revision changes reset the chain before
new-generation audio is admitted. Every play-owned DSP stage guards its **input**
with `sanitize_sample` (an output-only guard would leak the bypass and silence
fast paths); `IsolatorEq` also flushes denormal IIR state only once both input
and output fall below `f32::MIN_POSITIVE`, so unity and bypass stay bit-exact
(`a_decaying_tail_never_leaks_denormals`).

## Tempo & Key-Lock
`kithara_warp::StretchControls` (one `Arc` per deck, in
`PlayerConfig.timestretch`) owns the requested playback target, is seeded before
a track loads, and threads unchanged into the resident `Warp<S>`. `Queue`
delegates the target to the player; key-lock and backend are set on the same
handle. There is no second cached target.

**Target and effective rate are different surfaces and cannot silently diverge.**
The leading track publishes the deck's effective rate through `PlaybackShared`;
`Player::rate()` and `PlayerEvent::RateChanged` expose only that, so a fixed or
backendless resource reports `1.0`, and a paused deck or one with no leading
track reports `0.0`.

`WarpRenderer` runs at `ratio = 1/speed`; key-lock off (the constructed default)
gives `pitch = speed`, vinyl-style. Speed, key-lock, and backend apply live
mid-track with no reload, and each `PlayerTrack` re-reads the rate its resource
applies every render block, so a runtime change moves its media clock as well as
its DSP. **Without a backend - every wasm build - the same Warp slot stays in the
chain as an exact identity renderer**: output stays at 1.0. A `kithara-warp`
region boundary preserves backend history and is not a source discontinuity, and
stretch changes output frame count, never `AudioSpec.sample_rate`.

## Engine Load
`PlayerCore` creates one address-stable `EngineLoad` meter and passes the same
`Arc` through `TrackConfig` into `DecoderNode`. Neither `AudioConfig` nor
`PreparedAudioLane` carries it, so there is no second route for load to reach a
node.

## Configuration document

`PlayerConfig<S>` is this crate's one player configuration — tunables and
per-call wiring together — and `#[derive(Patch)]` generates `PlayerConfigPatch`,
what a document's `player:` section may say: `gapless_mode`,
`crossfade_duration`, `default_rate`, `max_slots`. The last is the engine's:
`PlayerConfig` owns it and hands it to `EngineConfig::builder`, so there is one
value behind both and `EngineConfig` needs no patch of its own.

Skipped, and therefore refused by name rather than dropped:
`auto_advance_enabled` and `prefetch_duration` (`Queue::new` overwrites both
unconditionally — the queue is their owner), `block_on_underrun` (only the
offline harness sets it; a real-time host callback can never block),
`eq_layout` (always a generator output; `Deck::build` derives it from
`AppConfig::eq_bands`, and a custom layout arrives at runtime through
`PlayerImpl::set_eq_layout`), and `sample_rate` (the Host owns the output rate
and refuses a player whose rate disagrees — see Engine Lifecycle below;
`Deck::build` reads it back off `Host::requested_sample_rate`, and the document
names it once, as `app.sample_rate`).

`ResourceConfig<S, B>` is where a document's HLS, file, and audio values wait for
the track that will use them. It carries them as patches — `HlsConfigPatch`,
`FileConfigPatch`, `AudioConfigPatch` — not as built configurations: an
`HlsConfig` needs a URL and a store and a `FileConfig` needs a source, so neither
can exist before a track does. `resource/build.rs` builds the real configuration
for the track and applies the patch onto it, so a knob either crate adds later
needs no edit here. The one value the file branch overwrites afterwards is
`extension`: the per-call `hint` and the extension derived from the source both
name this very track, so either outranks a document's blanket `file.extension`.

There is no `resource:` document section — `Document`'s `deny_unknown_fields`
refuses one, pinned by `a_resource_section_is_rejected`. The three patches arrive
from `kithara-app`'s top-level `audio:`, `hls:` and `file:` sections, and
`sources::build_resource_config` is the only construction site a document reaches.

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

`PlayerConfig.sample_rate` is mandatory and has no playback-layer default. It
is the Player's initial session-rate contract; Host attachment queries the
canonical dispatcher carried by `SessionBinding`, and that dispatcher is the
sole rate source for validation and register/start commands. Tests pass their
fixture rate explicitly.

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
one Player; `kithara-host` owns every concrete native or web session, the
Firewheel graph, and the root synchronization group; `kithara-warp` owns musical
coordinates and the `BeatGrid`, `WarpMap`, and `SyncGroup` protocols. **No concrete
production session constructor lives in this crate.** A Player
is constructed unbound and receives an opaque one-shot `SessionBinding` only
through `Host::insert`; decorators may only delegate it to their resident Player.
`TrackBinding` captures an owner-published `BeatGridSnapshot`; it neither creates
a grid identity nor converts analysis facts into a second representation.

`SessionDispatcher::consumer_wake_mode` is a **required** object-safe capability,
not a trait default, so a wrapper cannot silently erase an off-RT capability by
omission. `ConfigPrep` copies it into `ResourceConfig::consumer_wake_mode`,
so a player-managed resource has no second source of wake policy. An unset field
identifies a direct `Resource` reader and resolves to `ImmediateOffRt` while its
config becomes an `AudioConfig`. The
Host's `TransportCommitState` is the sole owner of when the session
beat-to-frame relation changes: a stream restart drops the transport snapshot
because the frame axis restarted, and the first block on the new axis reanchors
the preserved beat before publishing another.

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
