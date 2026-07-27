# kithara-ui - Context

Detailed contracts and invariants for the kithara-ui crate; the README remains the overview.

## Compiled String Ownership

Owner decision (b), 2026-07-19: every string retained by the compiled tree is interned in one
plain bounded `String` arena owned by `CompiledUi`. `UiConfig.max_arena_bytes` caps the string
buffer; growth uses `try_reserve`, and either the configured cap or an allocation failure returns
`UiDocError::ArenaFull`.

The compiled tree deliberately does not use kithara-bufpool. Its budget-charging `ensure_len`
requires `Default + Clone`, which `ExpandedNode` cannot provide. Growing a pooled structural
`Vec` through `push` would therefore bypass budget charging and make the budget inaccurate. A
pooled string buffer would also make `CompiledUi` non-`Clone` while occupying a churn-pool slot
for the preset lifetime. Bufpool remains the intended tool for the render hot path in a later
phase.

`InternId` is valid only within the `CompiledUi` that produced it. Recompilation rebuilds the
arena, so intern IDs must never be persisted in application messages or state. `ModularMsg`
continues to carry owned `String` paths, and hidden-module settings continue to store names.

`StrArena::resolve` is total. Spans describe whole strings appended to the buffer, so valid spans
land on UTF-8 boundaries and `String::get` resolves them without unsafe conversion. An unknown ID
or invalid span resolves to `""`; this is the documented handle behavior, not error recovery.

## Document And Compiled Layers

`BindingRef` and the typed `ControlNode` variants are the serde document inputs. `Binding` and
`ControlSpec` are their compiled forms. String payloads retained by `ControlSpec` are interned;
style, format, tone, and boolean fields remain typed values. This is an explicit layer split, not
a second source of domain truth: endpoint validation uses the typed document variant and the
substituted binding before the binding is interned.

The arena types live in `ids.rs` because they back compiled identifiers and strings, and keeping
them there preserves the crate's flat-directory budget.

The builtin skin is a compile-time asset; failing fast while initializing its `LazyLock` is the
sanctioned panic site for an invalid embedded document or color.

## Skin Ownership

`SkinDoc` is the canonical owner of every configurable rendering metric, including intrinsic
control sizes used by the toolkit-independent compiler. With the `render` feature, `Skin`
converts the complete document to iced colors while retaining the document for layout sizing.
The platform-specific monospace family remains code-owned because it describes font resource
availability rather than skin design.

## Wave View Ownership

Hero-wave zoom and playback position are host-owned scalar state. An optional `Wave.zoom` binding
reads the visible track fraction; wheel interaction emits `SetScalar` at `<wave-path>/zoom`, while
horizontal drag emits `SetScalar` at the wave path for the host-owned playback position. The
renderer keeps neither value and derives the centered zoom window from each read snapshot.

The hero wave dims the track left of the playhead with `SkinDoc.wave.played_alpha` mapped through
its zoom window; the bars style dims the full track with `SkinDoc.wave.overview_played_alpha`. The
micro style carries no playhead dimming.

## Text Tone Ownership

`Text` renders the content its `read` binding or `label` supplies; the separate optional `active`
binding is a Bool the host owns. `TextStyle::DeckLetter` reads it to switch to
`SkinDoc.text.deck_letter_active`, marking the focused deck.

## Meter Ownership

`Meter` reads one Scalar and draws it as a horizontal fill; it accepts no write, so the value
stays host-owned. `SkinDoc.meter` owns the track size, hairline frame, track colour and fill
colour. The fill is inset by the frame width on every side, so a full bar stops at the frame's
inner edge instead of covering it.

## Visualizer Ownership

`Vis` reads the host-owned preset as a Scalar and emits `SelectIndex` for preset changes. Its
master reaction level is derived from the `player.output.levels` Stereo snapshot as
`max(left, right) * volume`; the shader does not retain audio state. `SkinDoc.vis` owns the module
chrome and canvas metrics, while the embedded WGSL asset owns the three fixed render presets.

The render-only shader program stores an `Instant` per widget to derive animation time. A host
must keep requesting frames while the visualizer is visible; the gallery does so with its active
VIS-tab subscription. The shader implementation remains behind the `render` feature, so the
non-render wasm schema lane does not depend on wgpu or wall-clock state.

## Scoped Read Resolution

A read binding with a non-empty `with` map resolves through the canonical scoped key
`<endpoint>@<scope>=<value>[,<scope2>=<value2>...]`, scope names sorted lexically. The key is
built by `expand::scoped_key`, interned once at compile time, and carried by `Binding::key`
(`key == id` when the scope is empty). `render/tree/read.rs::resolve` passes exactly this key to
`Reads::get`; hosts key their read maps by the same form.

Widgets that read derived endpoints beyond their binding (`DeckSummary`, `Bpm`, `Time`,
`MiniWave`, `TrackList` column state) receive the binding's scope suffix (`@deck=a` or empty)
and append it to each derived endpoint, so `deck.track.title@deck=b` stays addressable per deck.
Host-global endpoints (`player.output.levels`, `ui.preset`, the visualizer clock) remain
unscoped.

## The Address Tree

`Reads::get` is the renderer's boundary and stays flat: one canonical key in, one value out.
`render::address` is how a host organises the answer behind it. A `Node` resolves one segment
into a child and reads its own value, and knows neither its siblings nor its parent, so no type
carries the whole vocabulary. `Walk` adapts a tree of them to `Reads`: it splits the key at `@`,
walks the dotted path segment by segment, and reads the leaf.

The scope selects an instance rather than naming a path segment, and the node that owns the
instances is the one that spends it — a leaf that reads a scope it does not own can answer for an
instance that does not exist. Both trait methods default to `None`, so a node answers only what
it claims; an address nobody claims reads as absent, which the renderer shows as its default.

A node whose value borrows from data built for the frame implements `Node` for `&Self`, so that
data outlives the walk; a node that only borrows longer-lived state implements it by value.

A module document id becomes a segment of `ui.module.<id>.collapsed`, so it must resolve as one:
`validate::check_module_id` rejects a module id containing `.`.

## Typed Control Schema

Each supported control is a structural `ControlNode` enum variant. RON deserialization owns field
validation, so the document layer has no string control discriminator, property map, or property
kind catalog. Common control fields are repeated in the serde variants because RON flattening is
not part of the schema contract.

Captions attached to a control — the fader's inline label, the knob's caption under its dial —
are document text carried on the control node; the skin owns their typography, and the knob's
caption is a full text role down to family and letter-spacing. A control without a caption renders
bare and callers compose no separate text node beside it; the knob's intrinsic size reserves the
caption row either way.

A waveform column is one bar wide for all three bands: low, mid and high are drawn from the
vertical centre over each other and nest by level, never by width, so a single `bar_width` and
`bar_gap` set the column pitch for both the deck wave and the overview row.

Controls take their size from the wrapper, not from themselves: a widget fills what
`size::control_size` or the document gives it. A widget that pins its own height would ignore the
document and break the row it sits in.

`Dim::Shrink` is the one rule the document layer cannot compose: the toolkit measures the content,
so `Bounds` treats it as an open axis and `Dim::from(Bounds)` never produces it. A shrunk node
therefore has to carry `Shrink` down to its own children — `render::tree::content_size` passes it
to the container, its frame overlay and its fill, because the first `Fill` inside a shrunk box
claims the whole row. Text measures its glyphs and takes alignment from that wrapper; a readout
that draws its own framed cell keeps filling the box the document gave it.

`validate::value_kinds` is the single owner of control read/write endpoint kinds. Intrinsic sizes
are selected exhaustively from `ControlSpec` and the supplied `SkinDoc` by
`size::control_size`; this remains available in non-render and wasm builds. Renderers match
`ControlSpec` directly and do not resolve a runtime control catalog.

## Module Chrome And Collapse Ownership

`ModuleDoc` owns optional shell labels, static assign labels, and footer binding plus a typed
`ChromeStyle`. `Frame` is the serde default and renders the plain module frame; `Plain` renders
only module content; `Full` adds the skin-owned 12e header, separators, and footer. Assign labels
render in the Full header immediately before its chevron.

Each layout module instance owns which outer frame sides are rendered, and whether the decorative
corner ticks are drawn at its top-left and bottom-right. These per-instance flags let adjacent
modules yield their shared edges to the layout grid, while the skin remains the owner of frame
thickness and color and of corner tick size, width, offset, and color. Corner ticks are opt-in:
a module instance that stays silent about them renders none.

A layout that declares `dragged` names what the pointer is carrying: while that
binding reads as text, the renderer draws it at the pointer over everything the
layout lays out. The ghost paints only — it captures no event and claims no
cursor — and asks for a redraw as the pointer moves, so following the pointer
costs the host no messages. `SkinDoc.drag` owns its box and type; where the box
sits relative to the pointer, and how much of the label fits in it, belong to
the widget.

A module that declares `drop` takes dragged items. It emits
`ControlAction::Drag(DragPhase::Over)` on `<instance>/drop` as the pointer
crosses its bounds and outlines itself while its `read` binding is true; the
`write` binding names the command the host runs on a drop. The renderer never
learns what is being dragged: the drag source reports its own start and release
on its own path, the host holds the item and decides what a drop means.

Collapse state remains host-owned. A Full module reads `Bool` from
`ui.module.<module-doc-id>.collapsed`; an absent value means expanded. Header activation emits
`UiEvent::ToggleModule(<module-doc-id>)`. The renderer does not retain or mutate collapse state,
and Frame or Plain modules ignore that endpoint.

## Window Chrome Ownership

A layout that sets `resize_edges` is framed by `render::tree` with the eight
drag zones a system border would have given it. They are laid over the content,
not beside it, so declaring them costs the layout no space; `SkinDoc.window`
owns their thickness, and the host maps each `WindowEdge` to its toolkit's own
resize direction.

`WindowDrag`, `TitleBar` and `WindowControls` paint no surface of their own and
take their size from the row that holds them, so the same controls sit in a 26px
gallery header and in the studio's 42px bar; the document declares the
background. `WindowDrag` is the bare drag surface a bar without a title needs;
`TitleBar` is the same surface with a label. Both emit on press, not on release
— a window drag only takes effect while the button is still held. Their glyphs
are canvas strokes drawn to the skin's icon size, which reads a little heavier
than the design's font glyphs at the same nominal size.

`TitleBar` and `WindowControls` are portable, binding-free controls. They emit typed
`UiEvent::Window(WindowCommand)` values; the host that owns the native window ID executes drag,
minimize, maximize, and close operations. `kithara-ui` owns their declarative schema and
skin-driven presentation, but never retains or mutates native window state.

## Track List Column Ownership

`TrackList` owns an ordered typed `Vec<TrackColumn>` and requires `Title` during compilation. The
renderer owns table geometry and cell presentation but not column visibility. When a
`columns_state` binding is present, the host may expose Bool reads at
`<binding-id>.<column-name>`; a missing derived endpoint means that column is visible. This keeps
one declarative column inventory while allowing library, playlist, and set-queue hosts to apply
presets without introducing renderer-owned mutable state.

The `Deck` column marks assignment and does not offer it: it shows the letters
the host put on the row and nothing when there are none. A row is a drag source
instead — pulling it past the gesture threshold emits
`ControlAction::Drag(DragPhase::Start)` on the control path, and the release
emits `DragPhase::Drop`. The gesture captures no event, so the row keeps its own
click and whatever the drag is released over sees the same release.

Column widths are host-owned through Scalar reads at `<binding-id>.width.<column-name>` and
`SetScalar` controls emitted at `<track-list-path>/width/<column-name>`. A missing width read uses
the skin default. The renderer retains only canvas drag state, clamps resizable fixed columns to
the skin minimum, and keeps the required Title column flexible with its skin-owned minimum.

## Browser Tree Ownership

`Tree` reads a borrowed flat row slice whose depth, branch state, selection, and presentation flags
are host-owned. The renderer never mutates or filters that state; activating any visible row emits
`ControlAction::SelectIndex` on the control path, and the host decides whether that index toggles a
branch or selects a leaf. `TreeSkin` owns the search, row, indentation, panel, and Zvuk context-bar
metrics. `ContextBar` keeps breadcrumb text read-only; optional scope items use a separate Scalar
read binding and emit `SelectIndex` on the control path so scope state remains host-owned.

## Application Consumer

The `kithara-app` GUI studio is the production consumer: it embeds its own layout and
module documents, implements `EndpointRegistry` and `Reads`, and maps `UiEvent` to app
messages (`crates/kithara-app/src/gui/studio_ui`). Builtin module docs under
`assets/modules/` remain the canonical presets consumed by the gallery modules page.
