# kithara-warp - Context

## Ownership

This crate owns the pure protocol used to align one beat map with another and
to compose maps through nested synchronization groups. It owns immutable
snapshots, coordinates, `WarpMap`, `SyncGroup`, topology operations, alignment
plans, cursors, and typed results. It also owns the resident identity `Warp<S>`
decorator, `WarpConfig`, and synchronous `WarpRenderer<S>`, which applies temporal
plans through the backend-neutral `kithara-stretch::ElasticEngine` contract on
native targets. Without an elastic backend, including on wasm, the same
renderer contract stays resident as an exact identity stage.

Host-axis values describe an ephemeral musical clock; they do not make this
crate the owner of the live Host, playback session, audio graph, or worker.
`SyncMember::Grid` accepts only `Send + Sync` leaf grids because an owning
topology operation may cross the wasm Worker-to-Host route. Nested group owners
remain `MaybeSend + MaybeSync`; the platform owner must split worker-bound
runtime state before transferring such a group.

## Boundaries

- `kithara-beat` owns neural beat detection and its raw model output.
- `kithara-analysis` owns progressive analysis and the cleaned, identity-free
  `BeatArtifact` consumed by a future calibrated grid adapter.
- `kithara-audio` owns decoded-audio source lifecycle, decoder-side sample-rate
  conversion, readiness, and the prepared producer seam.
- `kithara-stretch` owns backend DSP engines and their exact-span contract.
- `kithara-play` owns `PlayWorker`, `DecoderNode`, final output admission,
  post-Warp playback effects, engine-load measurement, Players, session state,
  and the audio graph.
- `kithara-assets` is the only production persistence path.

The crate must not depend on audio, play, host, assets, or analyzer runtime
types. `Warp<S>` is generic over its source, and `WarpRenderer<S>` is a synchronous
stage; neither makes this crate the owner of source lifecycle, playback
scheduling, or worker threads.

R7 keeps `Warp<S>` in identity mode. The production path shares live stretch
controls through it, but does not yet evaluate `WarpMap`, advance a runtime map
cursor, apply non-identity alignment, or turn render/presentation progress into
a `SyncGroup::acknowledge` call. The map and acknowledgement APIs remain pure
contracts for the later actuator integration.

## Configuration

`WarpConfig` is built with `bon`, uses `fieldwork` for read access, and carries
the shared `StretchControls` plus the non-zero render quantum in frames. The
resident `Warp<S>` passes that one config to its renderer; milliseconds are
derived diagnostics, not configuration. The identity renderer deliberately
ignores temporal intent while preserving the same stage contract. Every
receives the caller's configured `PoolRegion<S>`; it never creates a pool region. Source
ownership, cancellation, and worker resources remain in their canonical
configs and are not duplicated here.

Fixed-ratio sample-rate conversion remains owned by `kithara-decode`; it is not
a substitute Warp backend because resampling changes pitch. Targets without an
elastic backend report playback-rate capability as unavailable and preserve
decoded samples through the identity renderer.
