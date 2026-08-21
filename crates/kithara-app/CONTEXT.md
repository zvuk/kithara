# kithara-app — Context

Contracts and invariants for the kithara-app crate; the README is the overview.

## Broadcast service

The crate owns only the service wiring; the packaging and the origin belong to `kithara-broadcast`. A request
stays `Requested` until the session exposes its measured output rate, which configures both the ring and the
encoder and arms the single mix tap. App-root cancellation ends the origin and encoder; dropping the running
phase releases the tap.

Stopping blocks — it closes the feed, drains the encoder and joins the worker — so the toggle moves the handle
into an iced task and marks the service `Stopping`; only that task's completion message makes it `Off`. The GUI
tick polls `BroadcastHandle::status`, so a producer released by a device-rate change reaches `Off` the same way.

The canon puts this control in the app menu and a recorder module and the app has neither, so its REC cell sits
in the bar beside the CPU cell.

## UI host

The UI is a compiled `kithara-ui` document set and `gui::ui` is the host side of it. `Registry` declares every
endpoint the documents may bind, `AppUi::new` compiles both layout documents against it and returns
`UiDocError`, and a unit test compiles both, so a compile failure is a build defect rather than a runtime
condition. `compile_ui` merges `builtin::text_doc()` with `assets/ui/app-en.ktext.ron` before every compile;
that catalog holds only the window-manager menu words canon has no key for.

### Deck addressing

A deck is addressed by channel letter, and the letter is its position in the session. The letter appears in two
independent places — the control path (`deck-<letter>/`, `mixer/<letter>/`, `overview/<letter>/`) and the
`deck=` scope of a binding — so they must agree; a unit test walks the compiled tree asserting every
deck-scoped binding is addressed by the letter it reads. The micro bar is the one place carrying no letter:
`gui::ui::scope::MICRO_DECK` names the deck it drives and the same test admits `micro-bar/` only for that deck.
`scope` owns the mapping both ways (`deck_index`, `deck_letter`). Only lowercase ASCII maps to a position, and
the session bounds the letter, so one past the last deck resolves to nothing rather than a neighbour.

One position indexes every list a deck appears in: `Decks` is built from `DeckSet::decks()` in session order and
`ViewCache::refresh` resizes against `Decks`, so the address tree joins them by position alone. Changing the
session's deck list means rebuilding the view model with it; no key survives them drifting apart.

The first segment of a control path names a layout instance and `gui::ui::events::route` is the host's own list
of them, held against the documents by unit test, so an instance the documents mint cannot go unanswered.

### Drag, drop, focus

The library reports the drag it started on `library/tracks`, each deck module reports the pointer crossing it on
`deck-<letter>/drop`, and `ViewCache` joins them at the release; neither side addresses the other. The dragged
row and the hovered deck stay separate facts: hover only changes on a crossing, so clearing it with the drop
would strand a second drag onto the deck the pointer never left. While the drag is in flight `ui.drag.track`
names the row and the layout draws it at the pointer; the library's Deck column is a marker, not a control. A
drop focuses the deck it landed on: `deck.focused` marks it in the overview row and the keyboard's Delete
reaches it. `ViewCache` owns focus next to hover, both naming a deck by position, and the layout bounds both.

### Deck controls

- Tempo travel is `±TEMPO_RANGE` (50) percent, clamped where the deck applies it. The TEMPO block is the app's
  whole reach to the timestretch: one wheel surface, a detent anywhere on it moves tempo by `TEMPO_STEP` (1.5)
  percent, a held press drags the same way, a double click returns to zero.
- The block prints the playing BPM beside the tempo percent — the analysed BPM scaled by the tempo, an em dash
  while no analysis carries one. The deck's own bar prints the track BPM, which the tempo does not move.
- `deck.view.zoom_in` / `zoom_out` apply `kithara_ui::render::zoom_in` / `zoom_out` to the per-deck zoom the app
  owns, held to the bounds a wheel over the wave answers to; a deck no press has reached starts from
  `DEFAULT_ZOOM`.
- The stream-quality cell appears only where there is a choice: a deck with an empty `abr_variants` ladder
  answers `deck.stream.quality_hidden` and the cell leaves the row. The app supplies the rungs and owns the open
  flag per deck; a pick becomes `DeckMsg::SetQuality`, which sets the ABR mode on the deck's own
  `current_abr_handle` and mirrors it in the deck state.
- The mixer channel keeps the EQ; `EQ_MIN_DB` / `EQ_MAX_DB` are the knob's dB travel.

`Kithara` owns one EQ mode for the whole app; every deck keeps only its own desired gains in `UiState`.
Right-clicking either knob bank opens its host-owned pointer popover in `ViewCache`, which owns no product
state, and selecting a mode replaces every deck's player layout before the shared mode is committed. Which bank
the strip draws follows from one read, `deck.eq.bands`, and the popover marks its rung through
`deck.eq.selected` scoped by the band count the row stands for, so one mode answers one question per rung and a
third topology needs no third endpoint. Switching modes remaps each deck's middle gains independently: one MID
is copied to both four-band mids, and two mids are averaged on the way back.

### Window chrome and telemetry

The window opens without system decorations, so the bar of whichever shape draws is the chrome: each bar carries
a `drag` surface and a `window` control set, executed by `Message::Window` against the window this app opened,
and a unit test holds those four addresses as the whole of it. Resizing comes back through the layout's
`resize_edges` flag, which lays eight drag zones over the window's own edges; the platform window menu and
fullscreen stay out of reach. The CPU cell reports `engine.load` — the heaviest deck's audio-engine load, not
processor time — bound twice, as a `Meter` bar and as text, off the same per-frame deck snapshots as every other
deck read rather than off the live atomics.

### Reads and host-owned view state

`ReadRoot::new` is the one place the app state is cut into domains; each node below it holds one slice and
answers only its own addresses, so no type carries the whole vocabulary, and `Walk` turns the renderer's flat
endpoint key into a walk over it. A binding scope (`@deck=a`) selects an instance rather than naming a path
segment: the node owning the instances spends it. `ViewCache` owns what the renderer borrows but the model does
not hold: converted waveform columns, formatted strings, per-deck zoom and quality-menu flag, collapsed modules,
the hovered and focused deck, and the deck layout.

### Layout switching

Both deck layouts are compiled once at startup and the menu picks between them through `ui.layout.decks`. A
layout lays out a deck whole or not at all — body, overview row and channel strip appear together — and
`DeckLayout::decks` is the single owner of how many that is. Narrowing returns `Message::PauseHiddenDecks`,
pausing every deck the layout stops laying out, while the session keeps the deck and its queue so widening
brings it back where it was, paused. `ViewCache::set_layout` bounds the cache's two pointers into the deck list:
a hover on a dropped deck clears and a focus on one moves to the first laid-out deck, since a deck that no
longer renders reports no pointer crossing and nothing later would correct them.

The two layout documents repeat their frame because the layout schema has no include: only deck bodies, overview
rows and mixer channels may differ, and the bar and library nodes must stay identical. The switch is a menu row
per layout, addressed by the deck count it lays out, and `DeckLayout::from_decks` is the one place that count
becomes a layout. Unit tests hold both ends: the menu carries a row per layout, and pressing a row applies the
layout it names.

### Window shape

Each layout document is rooted in a `Split` measuring `Height` and declaring `(w: Fill, h: Fill)`, so the window
answers the box it is given whatever it draws. Its children arrive by band as the window grows taller: the micro
bar alone at the minimum, then the browser panel, then the overview row, then the deck row with the full bar in
place of the micro one. The deck row is itself a `Split` measuring `Width`, so the mixer and the second deck
arrive at the widths that draw them.

Every threshold is a number the compiled tree already answered, held by unit test rather than by comment: a
height band equals the summed minimum of the blocks that stand in it, and the browser's band equals the micro
bar's declared box plus the track list's own minimum. Width bands are held by the compiler
(`room::check_layout_cells`), so a band promising less room than its cell needs is a compile error. A block the
menu switches off still counts toward the bands above it, since `min_size` counts an `Optional` as standing — so
a window can draw the micro bar while the room a hidden pane would take is what keeps the decks out.

The micro bar is one module standing in one place, addressed as `micro-bar/<control>` and driving `MICRO_DECK`.
Its root `Row` measures `Width` and reveals each cell at the width it earns, with the menu, the play button, the
stretched place and the window controls standing at every width. The stretched place is two cells sharing one
band edge — a `WindowDrag` below it, the wave above — so the window stays movable at the bar's smallest and a
cell arriving beside the wave narrows it rather than taking it away.

`CompiledUi::min` is that bar's own `compiled_min`, since it is the only cell standing in the room the root
split settles on; `AppUi::window_min`
takes the larger of the two layouts' and `frontend::window_settings` hands it to `iced` as `min_size`.

## Track analysis cache

Source analysis is an expensive whole-track decode deriving the coloured waveform and an optional beat grid /
BPM estimate in one pass, so the combined result is memoized (`wave_cache.rs`). Each deck's `StateController`
spawns one `analysis::listen` task owning one `AnalysisController` and one `TrackAnalysisCache`, so the cache
needs no synchronization. Two identity spaces are kept separate on purpose:

- **`TrackId`** (session-scoped, from `kithara-events` via the queue) — stale guard for an in-flight run and the
  "still current" check at publish. Never persisted.
- **`AnalysisTarget`** (the track's `AssetStore` plus the `ResourceKey` derived by `ResourceConfig::asset_key`)
  — cross-session cache identity. `is_same` compares key *and* store, so one key in two stores is two entries.

`plan_analysis` returns `Skip` when the target is already displayed, `Serve` on a cache hit, and `Decode` only
on a genuine miss; only a `Decode` on the current track wipes the visible analysis first. `pump` refuses to
start a second run while one is in flight and clears the pending queue outright when the runner has no
analyzers. `on_track_changed` puts the current track at the front and preempts an in-flight background run;
`pending_order` is current-track-first, then list order. `AnalysisController::commit` caches the finished run
under its target and only then publishes it if the run's `TrackId` is still current — a stale run still lands in
the cache. A track whose source yields no `ResourceConfig` is skipped, and so is a source whose layout rejects the derived
key — after clearing the visible analysis when it is the current track. The `Option<AnalysisTarget>` seam means
an unkeyable run is decoded only while its track is current and is never cached.

The memory tier is bounded by `Consts::MAX_MEM_ENTRIES` (64) in insertion order; evicted entries are still
served from disk. An analysis with neither waveform nor beat grid is memoized in neither tier — it would
otherwise be served forever as emptiness. Disk reads probe `AssetStore::resource_state` first, because opening a
missing key would create it. The disk tier stores one blob per track as a resource of the track's asset scope
(`analysis/track.analysis`), so the artifact is evicted, moved and deleted together with the cached audio bytes.

Invalidation has two levers. `Consts::ANALYSIS_BYTES_VERSION` must be bumped whenever the blob framing or the
waveform / beat-grid encodings change. Configuration changes need no bump: `analysis_fingerprint` is written
into every blob and a mismatch is a miss, so `WAVEFORM_MAX_BUCKETS` and runtime beat-analysis tuning re-analyse
on their own. Because the identity is the source location and not the bytes, a file overwritten in place keeps
its entry until the version is bumped — acceptable for a library of stable files.

## Baked DRM secrets

`build.rs` bakes `app.yaml` provider secrets from the process env (workspace `.env` as fallback) into
`kithara_app::baked`. A missing `$KITHARA_*` reference degrades silently by design: the cipher key bakes as an
empty string, headers referencing the variable are omitted, and the binary compiles but the key server rejects
its requests. That is the intended mode for every build that never talks to a real key server, which is why it
is not a warning. Builds that do — the CI `network*` lanes, release pipelines — set `KITHARA_DRM_REQUIRE` (any
non-empty value): an upfront pass validates every env reference in `app.yaml`, not only the providers a given
lane exercises, and fails the build listing all missing variables.
