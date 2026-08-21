# kithara-app — Context

Contracts and invariants for the kithara-app crate; the README is the overview.

## Broadcast service

The crate owns only the service wiring; the packaging and the origin belong to `kithara-broadcast`.
A request stays `Requested` until the session exposes its measured output rate, which configures
both the ring and the encoder and arms the single mix tap. App-root cancellation ends the origin and
encoder; dropping the running phase releases the tap.

Stopping blocks — it closes the feed, drains the encoder and joins the worker — so the toggle moves
the handle into an iced task and marks the service `Stopping`; only that task's completion message
makes it `Off`. The GUI tick polls `BroadcastHandle::status`, so a producer released by a
device-rate change reaches `Off` through the same path.

The design canon puts this control in the app menu and a recorder module, and the app has
neither, so the owner placed the canon's REC cell in the bar beside the CPU cell.

## UI host

The UI is a compiled `kithara-ui` document set; `gui::ui` is the host side of it. `Registry` declares
every endpoint the documents may bind. `AppUi::new` compiles both layout documents against it and returns `UiDocError`,
which `GuiFrontend::run_loop` propagates; a unit test compiles both, so a compile failure is a build defect, not a runtime
condition.

`compile_ui` merges `builtin::text_doc()` with `assets/ui/app-en.ktext.ron`, the app's own catalog,
before every compile - the app document set uses canon text keys (`kithara-ui/CONTEXT.md`, "Text
Catalog Ownership") directly, and mints its own keys only for the four window-manager menu words
canon has no concept of (`menu.modules`, `menu.broadcast`, `menu.layout.single`,
`menu.layout.dual`).

### Deck addressing

A deck is addressed by channel letter, and the letter is its position in the session. The letter appears in two
independent places — the control path (`deck-<letter>/<control>` for a deck module, `mixer/<letter>/<control>` for a
channel strip, `overview/<letter>/<control>` for an overview row) and the `deck=` scope of a binding — so they must agree.
The micro bar is the one place carrying no letter: the document points it at a deck and `gui::ui::scope::MICRO_DECK`
routes `bar-micro/<control>` to the same one, so the two name it once each. The unit test below holds them together — a
binding scoped to any other deck under `bar-micro/` fails it.
`scope` owns the mapping both ways (`deck_index`, `deck_letter`); the library's Deck column prints what `deck_letter`
gives. A unit test walks the compiled tree asserting every deck-scoped binding is addressed by the letter it reads. Only
lowercase ASCII maps to a position; the session bounds the letter, so one past the last deck resolves to nothing rather
than a neighbour, and a position past the alphabet has no letter rather than a stand-in.

One position indexes every list a deck appears in: `Decks` is built from `DeckSet::decks()` in session order and
`ViewCache::refresh` resizes against `Decks`, so the address tree joins them by position alone. Changing the session's
deck list means rebuilding the view model with it; no key survives them drifting apart.

### Drag, drop, focus

The library reports the drag it started on `library/tracks`, each deck module reports the pointer crossing it on
`deck-<letter>/drop`, and `ViewCache` joins them at the release; neither side addresses the other. The dragged row and
the hovered deck stay separate facts: hover only changes on a crossing, so clearing it with the drop would strand a second
drag onto the deck the pointer never left. While the drag is in flight `ui.drag.track` names the row and the layout draws
it at the pointer. The library's Deck column is a marker, not a control.

A drop focuses the deck it landed on: `deck.focused` marks it in the overview row and the keyboard's Delete reaches it.
`ViewCache` owns focus next to hover; both name a deck by position and the layout bounds both.

### Deck controls

- Tempo travel is `±TEMPO_RANGE` (50) percent, clamped where the deck applies it. The TEMPO block is the app's whole
  reach to the timestretch: one wheel surface, a detent anywhere on it moves tempo by `TEMPO_STEP` (1.5) percent, a held
  press drags the same way, a double click returns to zero. The step is bounded against `TEMPO_RANGE` so the travel stays
  reachable by scrolling, not chosen for precision.
- The block prints the playing BPM beside the tempo percent: the analysed BPM scaled by the tempo, an em dash while no
  analysis carries one. The deck's own bar prints the track BPM, which the tempo does not move.
- `deck.view.zoom_in` / `zoom_out` apply `kithara_ui::render::zoom_in` / `zoom_out` to the per-deck zoom the app owns,
  held to the bounds the wave draws within — the same bounds a wheel over the wave answers to. A deck no press has reached
  yet starts from `DEFAULT_ZOOM`.
- The stream-quality cell appears only where there is a choice: a deck with an empty `abr_variants` ladder answers
  `deck.stream.quality_hidden` and the cell leaves the row. The app supplies the rungs and owns the open flag per deck;
  a rung is addressed by its slot, `auto` hands the choice back to the ladder, and a pick becomes `DeckMsg::SetQuality`,
  which sets the ABR mode on the deck's own `current_abr_handle` and mirrors it in the deck state.
- The mixer channel keeps the EQ; `EQ_MIN_DB` / `EQ_MAX_DB` are the knob's dB travel.

`Kithara` owns one EQ mode for the whole app; every deck keeps only its own
desired gains in `UiState`. Right-clicking either knob bank opens its host-owned
pointer popover in `ViewCache`; the popover itself owns no product state.
Selecting a mode replaces every deck's player layout before the shared mode is
committed. Three-band mode lays out HIGH / MID / LOW vertically, four-band mode
HIGH / HI-MID / LO-MID / LOW. Which bank the strip draws follows from one read,
`deck.eq.bands`, the band count the mode carries; the strip's `Adaptive` node
takes the four-band step from `4.0` up. The popover marks its rung through
`deck.eq.selected`, scoped by the `bands` count the row stands for, so one mode
answers one question per rung and a third topology needs no third endpoint.
Switching modes remaps each deck's middle gains independently: one MID is copied
to both four-band mids, and two mids are averaged on the way back.

### Window chrome and telemetry

The window opens without system decorations, so the bar of whichever shape draws is the chrome: `drag` is a `WindowDrag`
surface and `window` carries minimise, maximise and close, executed by `Message::Window` against the window this app
opened. Both bars carry the pair, under `bar/` and `bar-micro/`.
Resizing comes back through the layout's `resize_edges` flag, which lays eight drag zones over the window's own edges; the
platform window menu and fullscreen stay out of reach and the bar looks the same everywhere. The CPU cell reports
`engine.load` — the heaviest deck's audio-engine load, not processor time — bound twice, as a `Meter` bar and as text. It
comes off the same per-frame deck snapshots as every other deck read, never off the live atomics.

### Reads and host-owned view state

`ReadRoot::new` is the one place the app state is cut into domains; each node below it holds one slice and answers only
its own addresses, so no type carries the whole vocabulary; `Walk` turns the renderer's flat endpoint key into a walk over
it. A binding scope (`@deck=a`) selects an instance rather than naming a path segment: the node owning the instances
spends it.

`ViewCache` owns what the renderer borrows but the model does not hold: converted waveform columns, formatted strings
(tempo, playing BPM, remaining time, source subtitle, quality label), per-deck zoom and quality-menu flag, collapsed
modules, the hovered and focused deck, and the deck layout.

### Layout switching

Both deck layouts are compiled once at startup and the top bar picks between them through `ui.layout.decks`. A layout lays
out a deck whole or not at all — body, overview row and channel strip appear together — and `DeckLayout::decks` is the
single owner of how many that is. Narrowing returns `Message::PauseHiddenDecks`, pausing every deck the layout stops
laying out (a deck the user cannot see must not keep playing) while the session keeps the deck and its queue, so widening
brings it back where it was, paused. `ViewCache::set_layout` bounds the cache's two pointers into the deck list: a hover
on a dropped deck clears and a focus on one moves to the first laid-out deck — a deck that no longer renders reports no
pointer crossing, so nothing later would correct them.

The two layout documents repeat their frame because the layout schema has no include: only deck bodies, overview rows and
mixer channels may differ. The top bar and library nodes must stay identical, and the switch must offer one segment per
layout in the host's own index order — a unit test holds that last part.

### Window shape

Each layout document is rooted in an `Adaptive` node measuring `Width` and declaring `(w: Fill, h: Fill)`, so the window
answers the box it is given whichever shape it draws. From `1080.0` up it takes the branch the full tree stands in;
below it, the micro player. `1080.0` is the width at which both deck panes stay workable, and it is a document literal:
which deck layout renders belongs to `DeckLayout`, and a width may not reassign it.

The full tree asks for room on both axes, and nesting is how a document says "and": the wide step holds a second
`Adaptive` measuring `Height`, and the tree stands in that node's step. The threshold is the tree's own compiled minimum
height — `492.0` in `app.klayout.ron`, `450.0` in `app-single.klayout.ron` — which is the wide column summed: the `42.0`
bar, the overview row (`82.0` over two decks, `40.0` over one), the deck row's `158.0` and the library panel's `210.0`.
Under that height the gate draws the micro player, so a window reaching one axis and missing the other draws the shape
that fits it.

The micro player is itself an `Adaptive` over `Height`: from `252.0` up the bar stands over the library panel,
below it the bar stands alone. `252.0` is that two-pane form's own compiled minimum height — the bar's `42.0` plus the
track list's `210.0` — so the panel appears exactly when the window can hold a usable list. The height reaching a
measured node is bounded: `iced` lays the root out under `Limits::new(Size::ZERO, bounds)`, `Stack` forwards those
limits to its base layer, and `widgets/adaptive/measured.rs` narrows with `Fill`, which raises the minimum and leaves
the maximum the window set. The micro player stands in the document twice, as the root's base branch and as the height
gate's, and `bar-micro` stands in both branches of each copy, because `Adaptive` needs a base branch and the layout
schema has no include; a branch claims ids from its own set, so `micro`, `library-block` and `bar-micro` repeated across
branches are one address and one host handler. The panel keeps the `library-block` guard it carries in the wide shape,
so the room and the menu's LIBRARY cell both have to allow it and one answer does not survive the other shape.

A bar reveals the cells it has room for: its root `Row` measures `Width` and each cell that costs room stands behind a
threshold. The micro bar takes the wave from `350.0` and the remaining time from `440.0`; the full bar takes the CPU
block from `1120.0` and the wordmark from `1250.0`. A threshold below the width at which its bar exists at all would be
an always-true wrapper, which is why the broadcast cell in the full bar carries none.

The micro bar declares `(w: Range(min: 221.0, max: None), h: Fixed(42.0))`, the box of the cells that stand at every
width: the menu (36), play (68), the drag strip (36, held to the menu cell's width so the window stays movable at its
smallest), the seam before the window controls (1) and the controls themselves (80). `WINDOW_MIN_WIDTH` and
`WINDOW_MIN_HEIGHT` are held at or above that box by a unit test, because a window under it overflows the bar where
nobody is looking. That box is the window minimum outright, because every gate falls back to the micro player and the
micro player falls back to its bar: under `252.0` of height, at any width, the bar is the whole shape the window draws.

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

## Baked DRM secrets

`build.rs` bakes `app.yaml` provider secrets from the process env (workspace `.env` as fallback) into
`kithara_app::baked`. A missing `$KITHARA_*` reference degrades silently by design: the cipher key bakes as an empty
string, headers referencing the variable are omitted, and the binary compiles but the key server rejects its requests.
This is the intended mode for every build that never talks to a real key server (local dev, the workspace test gate),
which is why it is not a warning. Builds that do talk to one — the CI `network*` lanes, release pipelines — set
`KITHARA_DRM_REQUIRE` (any non-empty value): an upfront pass validates every env reference in `app.yaml` and fails the
build listing all missing variables, so a forgotten secret surfaces at build time instead of as a runtime key
rejection. The pass covers all env references, not only the providers a given lane exercises.
