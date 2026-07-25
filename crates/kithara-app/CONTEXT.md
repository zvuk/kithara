# kithara-app — Context

Detailed contracts and invariants for the kithara-app crate; the README is the overview.

## Studio UI Host

The studio is a compiled `kithara-ui` document set; `gui::studio_ui` is the host
side of it. `StudioRegistry` declares every endpoint the documents may bind, and
the documents are validated against it by a unit test, so `StudioUi::new` may
panic on a compile failure: it is a build defect, not a runtime condition.

The studio addresses decks by channel letter, and the letter is the deck's
position in the session. It appears in two independent places — the control path
and the `deck=` scope of a binding — so they must agree:

- `deck-<letter>/<control>` — a deck module;
- `mixer/<letter>/<control>` — a channel strip;
- `overview/<letter>/<control>` — an overview row.

`scope::deck_index` is the single owner of letter → position for both sides,
and a unit test walks the compiled tree asserting that every deck-scoped
binding is addressed by the letter it reads. Any lowercase ASCII letter maps to
a position; the session bounds it, so a letter past the last deck resolves to
nothing rather than to a neighbour.

Tempo travel is fixed at `TEMPO_RANGE` percent either way, clamped where the
deck applies it; there is no range selector and no reset control.

Reads are answered per frame by `StudioReads`, which borrows the app state and
`StudioCache`. The cache owns what the renderer borrows but the model does not
hold: converted waveform columns, formatted strings (tempo, remaining time,
source subtitle), per-deck zoom, collapsed modules, and the deck layout.

Both deck layouts are compiled once at startup and the top bar picks between
them through `ui.layout.decks`; the session owns the same decks either way, so
switching changes what is laid out and nothing else. The mixer keeps a channel
per session deck in both layouts — a hidden deck still plays, and its trim,
tone and mute stay reachable.

The two layout documents repeat their frame because the layout schema has no
include: only the deck body and the overview rows may differ between them. The
top bar, the mixer and the library nodes must stay identical, and the switch
must offer one segment per layout in the host's own index order — a unit test
holds that last part.

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
