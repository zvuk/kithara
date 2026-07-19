# kithara-app — Context

Detailed contracts and invariants for the kithara-app crate; the README is the overview.

## Track Analysis Cache

The DJ Studio source analysis is an expensive whole-track decode. It currently
derives the colored waveform and an optional source BPM estimate in one pass, so
the combined result is memoized (`wave_cache.rs`, owned by the single
`StateController` listener task). Two distinct identity spaces are kept
separate on purpose:

- **`TrackId`** (session-scoped `u64` from the queue) — the stale-guard for an
  in-flight run and the "still current" check at commit. Never persisted.
- **`AnalysisKey`** (source-derived, query/fragment-stripped URL/path, sha256 for
  the filename) — the cross-session cache key. The same source shares one entry
  and the disk tier survives restarts.

These never mix: `TrackId` answers "is this the same queue slot", `AnalysisKey`
answers "is this the same audio source". `plan_analysis` skips when the track is
already shown or in flight, serves a cache hit without wiping the visible
analysis, and only wipes + decodes on a genuine miss; `AnalysisController::commit`
publishes a completed run and `cache_completed` populates both tiers.

The disk tier stores one blob per track as a resource of the track's
`AssetScope` (`analysis/track.analysis`), so the artifact follows the track's
cache lifecycle: it is evicted, moved, and deleted together with the cached
audio bytes. A `TrackSource` variant with no stable source (the reserved
non-exhaustive seam) is in-memory-only by capability, not a fallback.

Invalidation is by `ANALYSIS_BYTES_VERSION` (kithara-app): bump it whenever
the blob encoding, waveform encoding, or `WAVEFORM_MAX_BUCKETS` change.
`AppConfig.beat_analysis` is part of each blob fingerprint through
`BeatAnalysisConfig::cache_tag`, so runtime beat-analysis tuning re-analyses
without a version bump. The
filename is a sha256 of the key — a `std` hasher is not stable across toolchain
versions and would orphan every blob. Because the key is the source location
and not the bytes, a file overwritten in place keeps its key until the version
is bumped (acceptable for a library of stable files).

## Modular Window Sizing

`AppConfig.window_sizing` owns the main window's fixed renderer chrome,
degenerate-axis floors, and comfortable initial-size scale. The modular preset
now owns its complete visible structure, so the default renderer chrome is zero
on both axes. A floor applies
only when the compiled tree has no intrinsic minimum on that axis; a positive
tree minimum remains authoritative and only receives renderer chrome. Initial
scale applies only to an open axis. A bounded axis opens at its minimum. If a
preset fails to compile, `ERROR_FALLBACK` supplies a degraded error-window body
size; this is an explicit degraded-mode default, not a fallback that masks a
state-resolution bug.
