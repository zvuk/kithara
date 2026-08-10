# kithara-app — Context

Contracts and invariants for the kithara-app crate; the README is the overview.

## Broadcast service

The crate owns only the service wiring; the packaging and the origin belong to `kithara-broadcast`.
The `--broadcast` flag records a `Requested` phase until the shared session exposes Firewheel's
measured output sample rate, then configures both the ring and encoder from that fact and arms the
single mix tap. The GUI owns the phase so the startup intent survives the interval before the first
deck creates the session output. App-root cancellation ends the origin and encoder; dropping the
running phase releases the mix tap.

The bar cell that drives it is a `StatusDot` reading `broadcast.on_air`, pressed through
`broadcast.toggle`. The design canon puts this control in the app menu and a recorder module, and
the studio has neither, so the owner placed it in the bar beside the CPU cell; its anatomy is the
canon's own REC cell.

Turning a running broadcast off moves its handle into an iced task immediately and marks the
service `Stopping`. The task delegates the blocking feed close, encoder drain, and worker join to
Tokio's blocking pool; only its completion message changes the service to `Off`. The existing GUI
tick polls `BroadcastHandle::status`, so a producer released by a device-rate change also moves the
service to `Off` without another timer.

## Studio UI host

The studio is a compiled `kithara-ui` document set; `gui::studio_ui` is the host side of it. `StudioRegistry` declares
every endpoint the documents may bind. `StudioUi::new` compiles both layout documents against it and returns `UiDocError`,
which `GuiFrontend::run_loop` propagates; a unit test compiles both, so a compile failure is a build defect, not a runtime
condition.

### Deck addressing

A deck is addressed by channel letter, and the letter is its position in the session. The letter appears in two
independent places — the control path (`deck-<letter>/<control>` for a deck module, `mixer/<letter>/<control>` for a
channel strip, `overview/<letter>/<control>` for an overview row) and the `deck=` scope of a binding — so they must agree.
`scope` owns the mapping both ways (`deck_index`, `deck_letter`); the library's Deck column prints what `deck_letter`
gives. A unit test walks the compiled tree asserting every deck-scoped binding is addressed by the letter it reads. Only
lowercase ASCII maps to a position; the session bounds the letter, so one past the last deck resolves to nothing rather
than a neighbour, and a position past the alphabet has no letter rather than a stand-in.

One position indexes every list a deck appears in: `Decks` is built from `DeckSet::decks()` in session order and
`StudioCache::refresh` resizes against `Decks`, so the address tree joins them by position alone. Changing the session's
deck list means rebuilding the view model with it; no key survives them drifting apart.

### Drag, drop, focus

The library reports the drag it started on `library/tracks`, each deck module reports the pointer crossing it on
`deck-<letter>/drop`, and `StudioCache` joins them at the release; neither side addresses the other. The dragged row and
the hovered deck stay separate facts: hover only changes on a crossing, so clearing it with the drop would strand a second
drag onto the deck the pointer never left. While the drag is in flight `ui.drag.track` names the row and the layout draws
it at the pointer. The library's Deck column is a marker, not a control.

A drop focuses the deck it landed on: `deck.focused` marks it in the overview row and the keyboard's Delete reaches it.
`StudioCache` owns focus next to hover; both name a deck by position and the layout bounds both.

### Deck controls

- Tempo travel is `±TEMPO_RANGE` (50) percent, clamped where the deck applies it. The TEMPO block is the studio's whole
  reach to the timestretch: one wheel surface, a detent anywhere on it moves tempo by `TEMPO_STEP` (1.5) percent, a held
  press drags the same way, a double click returns to zero. The step is bounded against `TEMPO_RANGE` so the travel stays
  reachable by scrolling, not chosen for precision.
- The block prints the playing BPM beside the tempo percent: the analysed BPM scaled by the tempo, an em dash while no
  analysis carries one. The deck's own bar prints the track BPM, which the tempo does not move.
- `deck.view.zoom_in` / `zoom_out` apply `kithara_ui::render::zoom_in` / `zoom_out` to the per-deck zoom the studio owns,
  held to the bounds the wave draws within — the same bounds a wheel over the wave answers to. A deck no press has reached
  yet starts from `DEFAULT_ZOOM`.
- The stream-quality cell appears only where there is a choice: a deck with an empty `abr_variants` ladder answers
  `deck.stream.quality_hidden` and the cell leaves the row. The studio supplies the rungs and owns the open flag per deck;
  a rung is addressed by its slot, `auto` hands the choice back to the ladder, and a pick becomes `DeckMsg::SetQuality`,
  which sets the ABR mode on the deck's own `current_abr_handle` and mirrors it in the deck state.
- The mixer channel keeps the EQ; `EQ_MIN_DB` / `EQ_MAX_DB` are the knob's dB travel.

`Kithara` owns one EQ mode for the whole studio; every deck keeps only its own
desired gains in `UiState`. Right-clicking either knob bank opens its host-owned
pointer popover in `StudioCache`; the popover itself owns no product state.
Selecting a mode replaces every deck's player layout before the shared mode is
committed. Three-band mode lays out HIGH / MID / LOW vertically, four-band mode
HIGH / HI-MID / LO-MID / LOW. Switching modes remaps each deck's middle gains
independently: one MID is copied to both four-band mids, and two mids are
averaged on the way back.

### Window chrome and telemetry

The window opens without system decorations, so the top bar is the chrome: `bar/drag` is a `WindowDrag` surface and
`bar/window` carries minimise, maximise and close, executed by `Message::Window` against the window this app opened.
Resizing comes back through the layout's `resize_edges` flag, which lays eight drag zones over the window's own edges; the
platform window menu and fullscreen stay out of reach and the bar looks the same everywhere. The CPU cell reports
`engine.load` — the heaviest deck's audio-engine load, not processor time — bound twice, as a `Meter` bar and as text. It
comes off the same per-frame deck snapshots as every other deck read, never off the live atomics.

### Reads and host-owned view state

`StudioRoot::new` is the one place the app state is cut into domains; each node below it holds one slice and answers only
its own addresses, so no type carries the whole vocabulary; `Walk` turns the renderer's flat endpoint key into a walk over
it. A binding scope (`@deck=a`) selects an instance rather than naming a path segment: the node owning the instances
spends it.

`StudioCache` owns what the renderer borrows but the model does not hold: converted waveform columns, formatted strings
(tempo, playing BPM, remaining time, source subtitle, quality label), per-deck zoom and quality-menu flag, collapsed
modules, the hovered and focused deck, and the deck layout.

### Layout switching

Both deck layouts are compiled once at startup and the top bar picks between them through `ui.layout.decks`. A layout lays
out a deck whole or not at all — body, overview row and channel strip appear together — and `DeckLayout::decks` is the
single owner of how many that is. Narrowing returns `Message::PauseHiddenDecks`, pausing every deck the layout stops
laying out (a deck the user cannot see must not keep playing) while the session keeps the deck and its queue, so widening
brings it back where it was, paused. `StudioCache::set_layout` bounds the cache's two pointers into the deck list: a hover
on a dropped deck clears and a focus on one moves to the first laid-out deck — a deck that no longer renders reports no
pointer crossing, so nothing later would correct them.

The two layout documents repeat their frame because the layout schema has no include: only deck bodies, overview rows and
mixer channels may differ. The top bar and library nodes must stay identical, and the switch must offer one segment per
layout in the host's own index order — a unit test holds that last part.

## Track analysis cache

Source analysis is an expensive whole-track decode deriving the coloured waveform and an optional beat grid / BPM estimate
in one pass, so the combined result is memoized (`wave_cache.rs`). Each deck's `StateController` spawns one
`analysis::listen` task owning one `AnalysisController` and one `TrackAnalysisCache`, so the cache needs no
synchronization.

Two identity spaces are kept separate on purpose:

- **`TrackId`** (session-scoped, from `kithara-events` via the queue) — stale guard for an in-flight run and the "still
  current" check at publish. Never persisted.
- **`AnalysisTarget`** (the track's `AssetStore` plus the `ResourceKey` derived by `ResourceConfig::asset_key` for
  `AssetResource::Named { namespace: "analysis", name: "track.analysis" }`) — cross-session cache identity. `is_same`
  compares key *and* store, so one key in two stores is two entries.

`plan_analysis` returns `Skip` when the target is already displayed, `Serve` on a cache hit (published without wiping the
visible analysis), and `Decode` only on a genuine miss; only a `Decode` on the current track wipes the visible analysis
first. `pump` refuses to start a second run while one is in flight and clears the pending queue outright when the runner
has no analyzers. `on_track_changed` puts the current track at the front and preempts an in-flight background run so the
visible deck wins; `pending_order` is current-track-first, then list order.

`AnalysisController::commit` caches the finished run under its target and only then publishes it, if the run's `TrackId`
is still current — a stale run still lands in the cache. A track whose source yields no `ResourceConfig` is skipped; a
source whose layout rejects the derived key is skipped after clearing the visible analysis when it is the current track.
The `Option<AnalysisTarget>` seam means an unkeyable run is decoded only while its track is current and is never cached.

The memory tier is bounded by `Consts::MAX_MEM_ENTRIES` (64) in insertion order; evicted entries are still served from
disk. An analysis with neither waveform nor beat grid is memoized in neither tier — it would otherwise be served forever
as emptiness. Disk reads probe `AssetStore::resource_state` first, because opening a missing key would create it. The disk
tier stores one blob per track as a resource of the track's asset scope (`analysis/track.analysis`), so the artifact is
evicted, moved and deleted together with the cached audio bytes.

Invalidation has two levers. `Consts::ANALYSIS_BYTES_VERSION` must be bumped whenever the blob framing or the waveform /
beat-grid encodings change. Configuration changes need no bump: `analysis_fingerprint`
(`wave=native:max<WAVEFORM_MAX_BUCKETS>;beat=<BeatAnalysisConfig::cache_tag>`, `beat=off` when disabled) is written into
every blob and a mismatch is a miss, so `WAVEFORM_MAX_BUCKETS` and runtime beat-analysis tuning re-analyse on their own.
Because the identity is the source location and not the bytes, a file overwritten in place keeps its entry until the
version is bumped (acceptable for a library of stable files).
