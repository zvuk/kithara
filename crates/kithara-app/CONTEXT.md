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

`scope` is the single owner of the mapping both ways: `deck_index` for letter
to position, `deck_letter` for position to letter, which is what the library's
Deck column prints. A unit test walks the compiled tree asserting that every
deck-scoped binding is addressed by the letter it reads. Any lowercase ASCII
letter maps to a position; the session bounds it, so a letter past the last
deck resolves to nothing rather than to a neighbour, and a position past the
alphabet has no letter rather than a stand-in.

One position indexes every list a deck appears in. `Decks` is built from
`DeckSet::decks` in session order and `StudioCache` is refreshed against
`Decks`, so the view model and the cache line up with the session by
construction and the address tree joins them by position alone. Changing the
session's deck list means rebuilding the view model with it; the lists carry no
key that would survive them drifting apart.

A track reaches a deck by being dragged onto it. The library reports the drag
it started on `library/tracks`, every deck module reports the pointer crossing
it on `deck-<letter>/drop`, and `StudioCache` joins them at the release: the row
it carried and the deck under the pointer. Neither side addresses the other.
While the drag is in flight `ui.drag.track` names the row it carries, and the
layout draws that name at the pointer.
The two facts are kept apart — the dragged row and the hovered deck — because
hover only changes on a crossing: clearing it with the drop would strand a
second drag onto the deck the pointer never left. The library's Deck column is
a marker, not a control: it shows the letters of the decks a row is loaded on.

The deck a drop lands on takes the studio focus: `deck.focused` marks it in the
overview row and the keyboard's Delete reaches it. `StudioCache` owns the focus
next to the hover: both name a deck by position, and the layout bounds both.

Tempo travel is fixed at `TEMPO_RANGE` percent either way, clamped where the
deck applies it. The deck's TEMPO block is the studio's whole reach to the
timestretch: the block is one wheel surface, a detent over any part of it moves
the tempo by `TEMPO_STEP` percent, a held press drags it the same way, and a
double click returns it to zero. The step is what makes the travel reachable by
scrolling at all, so it is bounded against `TEMPO_RANGE` rather than chosen for
precision. The mixer channel keeps the EQ the design canon reserves it for.

The block prints the playing BPM beside the tempo percent — the analysed BPM
scaled by the tempo, an em dash while no analysis carries one. The percent is
the accented reading and the BPM follows it dimmed. The deck's own bar prints
the track's BPM, which the tempo does not move.

Beside the block the transport carries the pair of zoom buttons the design canon
gives it, widening then narrowing left to right. They trigger
`deck.view.zoom_in` and `deck.view.zoom_out`, which the studio applies to the
per-deck zoom it owns; `kithara_ui::render::zoom_in` and `zoom_out` decide what
a press is worth and hold the scale to the bounds the wave draws within, the
same bounds a wheel over the wave answers to. A deck no press has reached yet
starts from `DEFAULT_ZOOM`, which is what the wave shows until then.

The studio window opens without system decorations, so the top bar is the
window chrome: its empty middle is a `WindowDrag` surface, and the cell on its
right carries minimise, maximise and close. `Message::Window` executes those
against the window this app opened. Resizing comes back through the layout's
`resize_edges` flag, which lays eight drag zones over the window's own edges.
The platform's own window menu and fullscreen stay out of reach, and the bar
looks the same on every platform.

The top bar's CPU cell reports `engine.load` — the heaviest deck's audio-engine
load, not the machine's processor time. It carries the design canon's CPU label
because that is the reading it gives, and shows the same endpoint twice: a
`Meter` bar beside the percentage. The value comes off the same per-frame deck
snapshots every other deck read comes off, never off the live atomics.

Reads are answered per frame by an address tree. `StudioRoot::new` is the one
place the app state is cut into domains; each node below it holds a single slice
and answers only its own addresses, so no type carries the whole vocabulary.
`Walk` turns the renderer's flat endpoint key into a walk over that tree, and a
binding scope (`@deck=a`) selects an instance rather than naming a path segment:
the node that owns the instances is the one that spends it. `StudioCache` owns
what the renderer borrows but the model does not hold: converted waveform
columns, formatted strings (tempo, playing BPM, remaining time, source
subtitle), per-deck zoom, collapsed modules, the hovered and focused deck, and
the deck layout.

Both deck layouts are compiled once at startup and the top bar picks between
them through `ui.layout.decks`. A layout lays out a deck whole or not at all:
its body, its overview row and its channel strip appear together, and
`DeckLayout::decks` is the single owner of how many that is. Narrowing the
layout pauses every deck it stops laying out — a deck the user cannot see must
not keep playing — while the session keeps the deck and its queue, so widening
the layout brings it back where it was, paused. The same switch bounds the
cache's two pointers into the deck list: a hover on a dropped deck clears and a
focus on one moves to the first laid-out deck. A deck that no longer renders
reports no pointer crossing, so nothing later would correct them.

The two layout documents repeat their frame because the layout schema has no
include: only the deck bodies, the overview rows and the mixer channels may
differ between them. The top bar and the library nodes must stay identical, and
the switch must offer one segment per layout in the host's own index order — a
unit test holds that last part.

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
