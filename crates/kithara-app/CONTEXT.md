# kithara-app — Context

Contracts and invariants for the kithara-app crate; the README is the overview.

## Buffer-pool ownership

`pools::AppPools` is the desktop composition schema. `main` builds one
`PoolRegion<AppPools>` and gives the same facade to the asset store, HTTP
client, playback worker, queues, and analysis cache. Its `u8` and `f32` slots
compete under one 256 MiB hard cap; startup allocation is declared in the
schema rather than warmed later by a component.

## Recording asset adapter

`AssetPartSink` is the application composition seam between storage-neutral
`kithara-record` and the canonical `AssetStore`. It acquires one phase-typed
writer, maps random-access container writes to it, and publishes only through
consuming `commit(final_len)`. Abort closes the writer,
then removes that relative resource through AssetStore's canonical deletion
channel: a cancelled or failed recording leaves no active partial asset.
Encoding and recording crates never receive a filesystem path or an AssetStore.

## Broadcast service

The crate owns only the service wiring; `main` builds the complete `BroadcastConfig` with the app's shared
worker, pools, and cancellation parent. Packaging, the bounded intake, and the origin belong to
`kithara-broadcast`. A request stays `Requested` until the Host exposes its measured output rate, which replaces
only the configured sample rate before `BroadcastOutput` is installed in the single Host `OutputGroup`. App-root
cancellation ends the origin and encoder; stopping the running phase releases the output group before the
encoder drains.

Stopping blocks - it closes the bounded intake and waits for the encoder tail - so the toggle moves the handle
into an iced task and marks the service `Stopping`; only that task's completion message makes it `Off`. The GUI
tick polls `BroadcastHandle::status`, so an output released by a device-rate change reaches `Off` the same way.

The canon puts this control in the app menu and a recorder module and the app has neither, so its REC cell sits
in the bar beside the CPU cell.

## UI host

The UI is a compiled `kithara-ui` document set and `gui::ui` is the host side of it. `Registry` declares every
endpoint the documents may bind, `AppUi::new` compiles both layout documents against it and returns
`UiDocError`, and a unit test compiles both, so a compile failure is a build defect. `compile_ui` merges `builtin::text_doc()` with `assets/ui/app-en.ktext.ron` before every compile;
that catalog holds only the window-manager menu words canon has no key for.

### Where the UI package is read from

`AppConfig.ui_package` names the UI package folder: `main` defaults it to `assets/ui` beside the executable,
where a release lays its documents out, and `--ui-package` overrides it. `AppUi::new` reads that folder over
what the build embeds, so a document changed on disk changes the interface at the next start without a rebuild.

A path that does not exist means no package was laid out and the build's own documents draw, as when running
from a build directory. Anything else that stops the folder being read - a permission,
a manifest that fails the `kithara-ui` contract - stops the application rather than drawing the built-in
one. This is the one place the application accepts a missing input as an answer: the package is
optional configuration, a user-facing default rather than a state-resolution fallback.

`gui::ui::package::Package` is the single owner of one loaded package: the resolver it is read through, the
screens it answered for, and the skin and catalog it dresses them in. Both hosts read that one value: the
iced host paints with `Package::skin`, the retained host builds its window `Config` from the same resolver and
catalog. Two packages drawing one application is the failure this shape prevents.

The application asks the package for `deck-single` and `deck-dual` by role, and `Package` resolves both once.
A manifest may also name a skin document and a caption catalog; a skin is what lets a package change how the
application looks without a rebuild. A manifest naming neither wears the built-in skin and words: a package
carrying pages and nothing else, declared optionality rather than a fallback.

`Package::REQUIRED` is what a package must answer for, checked once each screen compiles:

- `deck-a/play`, the only path that starts and stops playback; without it a player cannot play.
- `deck-a/wave`, the only path that moves within a track; without it a track starts and never moves.

Everything else is the package's own business; the minimum is checked because a screen missing a path still
compiles and draws, and a press landing nowhere reads as a dead button rather than a package defect.

Reading the package from disk costs 1.7 ms once at start; compilation dominates, so the resolver caches what it
read rather than indexing what it might.

### Deck addressing

A deck is addressed by channel letter, and the letter is its position in the session. The letter appears in two
independent places, the control path (`deck-<letter>/`, `mixer/<letter>/`, `overview/<letter>/`) and the
`deck=` scope of a binding, so they must agree; a unit test walks the compiled tree asserting it. The micro bar is the one place carrying no letter:
`gui::ui::scope::MICRO_DECK` names the deck it drives and the same test admits `micro-bar/` only for that deck.
`scope` owns the mapping both ways (`deck_index`, `deck_letter`): only lowercase ASCII maps to a position, and
the session bounds the letter, so one past the last deck resolves to nothing rather than a neighbour.

One position indexes every list a deck appears in: `Decks` is built from `DeckSet::decks()` in session order and
`ViewCache::refresh` resizes against `Decks`, so the address tree joins them by position alone. Changing the
session's deck list means rebuilding the view model with it.

Each `Deck` owns one cancellation token below the app shutdown token: its player and queue get independent
children, its state controller and analysis listener share a third. Dropping a deck cancels that subtree and
nothing else.

The first segment of a control path names a layout instance; `gui::ui::events::route` is the host's list of
them, held against the documents by unit test, so an instance the documents mint cannot go unanswered.

### Drag, drop, focus

The library reports the drag it started on `library/tracks`, each deck module reports the pointer crossing it on
`deck-<letter>/drop`, and `ViewCache` joins them at the release; neither side addresses the other. The dragged
row and the hovered deck stay separate facts: hover changes only on a crossing, so clearing it with the drop
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
- The mixer channel keeps the EQ; `GainDb` carries the knob's dB travel.

The deck module is retained-hosted, but the tempo surface stays on iced: the engine observes each decoded event first,
and an unanswered wheel event reaches the same child unchanged. The Hero Wave and five transport buttons have engine
descriptors; the tempo row deliberately does not.

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
and a unit test holds those four addresses as the whole of it. Resizing comes through the layout's `resize_edges`
flag, eight drag zones over the window's edges; the platform window menu and fullscreen stay out of reach. The CPU cell reports `engine.load`, the heaviest deck's audio-engine load rather
than processor time, bound twice, as a `Meter` bar and as text, off the per-frame deck snapshots rather than the
live atomics.

### Reads and host-owned view state

`ReadRoot::new` is the one place the app state is cut into domains; each node below it holds one slice and
answers only its own addresses, so no type carries the whole vocabulary, and `Walk` turns the renderer's flat
endpoint key into a walk over it. A binding scope (`@deck=a`) selects an instance rather than naming a path
segment: the node owning the instances spends it. `ViewCache` owns what the renderer borrows but the model does
not hold: converted waveform columns, formatted strings, per-deck zoom and quality-menu flag, collapsed modules,
the hovered and focused deck, and the deck layout. Smaller views sit beside them, one owner each:
`MenuState` (which menu group is open), `Modules` (which pane the menu switched off), `WindowState` (what the
single window reports), `LibraryView` (the library's own query and scope) and `StageView` (the tempo-map window
edges and the visualisation preset, answered by `TempoNode`/`VisNode`). A view is read through `ReadRoot` and
written only by `ui::events`; nothing else holds a second copy.

`AppUi` carries the compiled document set and a `Clock`. The clock answers `ui.clock.seconds`, so a frame is
reproducible from the state that produced it: `update` steps it once per tick and both hosts read the same
value rather than sampling wall clocks of their own.

### Layout switching

Both deck layouts are compiled once at startup and the menu picks between them through `ui.layout.decks`. A
layout lays out a deck whole or not at all (body, overview row and channel strip appear together) and
`DeckLayout::decks` is the single owner of how many that is. Narrowing returns `Message::PauseHiddenDecks`,
pausing every deck the layout stops laying out; the session keeps the deck and its queue, so widening brings
it back where it was, paused. `ViewCache::set_layout` bounds the cache's two pointers into the deck list:
a hover on a dropped deck clears and a focus on one moves to the first laid-out deck, since a deck that no
longer renders reports no pointer crossing.

The two layout documents repeat their frame because the layout schema has no include: only deck bodies, overview
rows and mixer channels may differ, and the bar and library nodes must stay identical. The switch is a menu row
per layout, addressed by the deck count it lays out, and `DeckLayout::from_decks` is the one place that count
becomes a layout. Unit tests hold both ends: a row per layout in the menu, and a press applying the layout it names.

### Window shape

Each layout document is rooted in a `Split` measuring `Height` and declaring `(w: Fill, h: Fill)`, so the window
answers the box it is given whatever it draws. Its children arrive by band as the window grows taller: the micro
bar alone at the minimum, then the browser panel, then the overview row, then the deck row with the full bar in
place of the micro one. The deck row is itself a `Split` measuring `Width`, so the mixer and the second deck
arrive at the widths that draw them.

Every threshold is a number the compiled tree already answered, held by unit test: a
height band equals the summed minimum of the blocks standing in it, and the browser's band equals the micro
bar's declared box plus the track list's minimum. Width bands are held by the compiler
(`room::check_layout_cells`): a band promising less room than its cell needs is a compile error. A block the
menu switches off still counts toward the bands above it, since `min_size` counts an `Optional` as standing, so
a window can draw the micro bar while the room a hidden pane would take keeps the decks out.

The micro bar is one module standing in one place, addressed as `micro-bar/<control>` and driving `MICRO_DECK`.
Its root `Row` measures `Width` and reveals each cell at the width it earns, with the menu, the play button, the
stretched place and the window controls standing at every width. The stretched place is two cells sharing one
band edge — a `WindowDrag` below it, the wave above — so the window stays movable at the bar's smallest and a
cell arriving beside the wave narrows it rather than taking it away.

`CompiledUi::min` is that bar's own `compiled_min`, since it is the only cell standing in the room the root
split settles on; `AppUi::window_min` takes the larger of the two layouts' and `frontend::window_settings` hands
it to `iced` as `min_size`.

## Track analysis

Progressive source analysis derives the coloured waveform and an optional beat grid / BPM estimate from decoded
ranges. `AnalysisService` is the one owner of analysis values: the `TrackAnalysisRunner` (one pass in flight),
the two-tier `TrackAnalysisCache`, the `AnalysisPersistence` client, and one entry per analysed resource. The GUI
frontend creates both once and hands a cloneable `AnalysisHandle` to every deck `StateController`. Requests arrive on one channel; values leave on one `watch` channel per entry.
Nothing else writes an analysis value. Two identities stay separate: `TrackId` (session-scoped, never persisted)
names the track a request is about and where a pass hands its producer (`attach_observer`); `AnalysisTarget`
(the track's `AssetStore` plus the `ResourceKey` from `ResourceConfig::asset_key`) is the entry and cache
identity, and `is_same` compares key *and* store.

`subscribe(queue, track_id, source, axis)` resolves the target, points the entry at the requester, seeds it from
memory then disk for that axis, and returns its receiver; a source with no resource or a rejected key gets a
closed receiver. `warm(queue, track_ids, axis)` puts a library list in line behind
every held entry without reading the cache and leaves a held entry's requester alone; the seed is read when the
pass opens. Readiness has one rule, `complete_for`: the extent is covered and every artifact the
configuration's fingerprint expects is present. An entry that is not complete is scheduled; a resumable seed
resumes, any other opens a fresh pass above the revision the entry holds, so revisions stay monotonic per token
across passes. A checkpoint the runner rejects is no seed: the pass opens fresh. An entry is done (`Stage::Ended`
on that axis) only when its pass closed on a complete or a settled value; a close without a value or on an
unsettled one leaves it `Idle`, and the next subscribe or warm schedules it again. `Queued` means in line; with
no analyzer compiled in nothing is queued. The player's current track takes no part in this path.

The runner serves held entries (a live receiver) before warm ones, each in request order. A subscribe whose
entry is not the running one ends a background pass (`runner.clear()`) and puts it back in line; a pass for a
held entry is never ended by another held entry, which waits. Every request names the engine rate axis; a pass
on another axis is ended and its entry put back in line.

A deck observes the track it shows, `UiState::current_track_index`, whether or not it plays. On `TrackAdded` /
`TrackRemoved` the listener keeps that index naming a track: the player's current item, else the track shown
while the list still has it, else the first. It re-subscribes on `QueueEvent::CurrentTrackChanged`,
`EngineEvent::Started`, `SessionEvent::RouteChanged`, `TrackAdded` and `TrackRemoved`, warms the list on the last
two, and drops its receiver before it asks for the next track. A lagged event bus is a resync: the list and the
current index are read from the queue, then the deck follows and warms again.

Every revision a pass publishes goes to the entry's sender, `cache.put`, and `persistence.try_store`; a full
persistence queue drops only that intermediate write. When the pass's channel closes, its last value is cached
and sent, and the service awaits `persistence.store` before the runner takes the next entry. The cache tiers are
the store: an entry no deck holds drops its value when its run ends and is seeded again on the next subscribe.
The deck listener mirrors its receiver into `UiState::analysis` when the `(token, revision)` differs from what
is shown; `DeckCache::refresh_wave` redraws on the same pair.

The memory tier holds `Consts::MAX_MEM_ENTRIES` (64) in insertion order; evicted entries are served from disk. An analysis with neither waveform nor beat grid is memoized in neither tier. Disk reads probe
`AssetStore::resource_state` first: opening a missing key would create it. The disk tier stores one
progressive `AnalysisFile` per track in the track's asset scope (`analysis/track.analysis`), so it is evicted,
moved and deleted with the audio bytes. Its header and completion index identify covered chunks; each
committed generation replaces the payload. Restore validates fingerprint, source rate, extent and chunk duration
before resuming only missing ranges.

Invalidation has two levers: the composite codec version in `kithara-analysis`, bumped whenever its framing or
the waveform / beat-grid encodings change, and `analysis_fingerprint`, written into every blob so a
configuration mismatch is a miss and `waveform_max_buckets` or beat-analysis tuning re-analyse on their own.
The identity is the source location, not the bytes: a file overwritten in place keeps its entry until the
version is bumped, acceptable for a library of stable files.

## Baked DRM secrets

`build.rs` bakes `app.yaml` provider secrets from the process env (workspace `.env` as fallback) into
`kithara_app::baked`. A missing `$KITHARA_*` reference degrades silently by design: the cipher key bakes as an
empty string, headers referencing the variable are omitted, and the binary compiles but the key server rejects
its requests: the intended mode for every build that never talks to a real key server, so it is not a warning.
Builds that do (the CI `network*` lanes, release pipelines) set `KITHARA_DRM_REQUIRE` (any non-empty value): an
upfront pass validates every env reference in `app.yaml`, not only the providers a given lane exercises, and
fails the build listing all missing variables.
