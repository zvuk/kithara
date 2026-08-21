# kithara-ui - Context

Contracts and invariants for the kithara-ui crate; the README stays the overview.

## Compiled String Ownership

Every string retained by the compiled tree is interned in one bounded `String` arena owned by
`CompiledUi`. `UiConfig.max_arena_bytes` caps it; growth uses `try_reserve`, and either the cap or
an allocation failure returns `UiDocError::ArenaFull`.

- No kithara-bufpool here: budget-charging `ensure_len` needs `Default + Clone`, which `ExpandedNode`
  cannot provide, and a pooled string buffer would make `CompiledUi` non-`Clone` while holding a
  churn-pool slot for the preset lifetime.
- `InternId` is valid only within the `CompiledUi` that produced it; a recompile rebuilds the arena.
  Never persist one in application messages or state - host-facing paths stay owned `String`s.
- `StrArena::resolve` is total: spans cover whole appended strings, so valid spans land on UTF-8
  boundaries. An unknown ID or invalid span resolves to `""` - handle behaviour, not error recovery.

## Document And Compiled Layers

`BindingRef` and the typed `ControlNode` variants are the serde document inputs; `Binding` and
`ControlSpec` their compiled forms. `ControlSpec` string payloads are interned; style, format, tone
and boolean fields stay typed. A layer split, not a second source of domain truth: endpoint
validation runs on the typed document variant and the substituted binding before interning.
Arena types live in `ids.rs` because they back compiled identifiers and strings. The builtin skin is
a compile-time asset; the `LazyLock` in `builtin::skin_doc` / `builtin::skin` is the sanctioned panic
site for an invalid embedded document or colour.

## Skin Ownership

`SkinDoc` owns every configurable rendering metric, including the intrinsic control sizes the
toolkit-independent compiler reads. With `render`, `Skin::resolve` converts the document to iced
colours and keeps it behind `Skin::document()` for layout sizing. The platform-specific monospace
family stays code-owned in `render/fonts.rs` - font availability, not skin design.

`Skin::resolve` also copies every sub-skin (`button`, `cell`, ... one per widget family) onto `Skin`
itself, so each field's value lives twice: once flat on `Skin` for zero-indirection hot-path render
access (`skin.button()`), once nested inside the retained `document: SkinDoc` for round-trip (the
raw doc a "reload"/"save skin" flow reads back). That is why `render/skin.rs` is listed in
`field_passthrough.exempt_files` in `.config/arch/thresholds.toml` - the check's "duplicates a field
already reachable through `self.document`" is structurally correct but the duplication is the design,
not an oversight; collapsing it back to `self.document().button` would put the doc's indirection back
on every render call.

The palette is the single colour vocabulary: a skin section names a `ColorRole`, never a hex, and
only `PaletteDoc::validate` parses one. Alpha stays a skin field beside the role it applies to, in
the shape `track_alpha`, `played_alpha` and `ShadowSkin.alpha` already carry.

A `ColorRole` is an alias for a value; several role names do not carry the design token they sound
like. Select a role by the value it holds:

| role | hex | design token |
| --- | --- | --- |
| `BgFooter` | `#1b1b32` | `panel2` |
| `BgSelect` | `#26264a` | `select` |
| `BgPanel2` | `#26264a` | none - duplicate of `BgSelect` |
| `LineInner` | `#2a2a4c` | `lineDim` |
| `LineSoft` | `#2a2a4c` | none - duplicate of `LineInner` |
| `LineDim` | `#242442` | none |
| `AccentSoft` | `#bb94422e` | none - and no consumer |

Two pins guard the palette, one property each. `palette_holds_exactly_the_declared_roles` in
`doc/skin/document.rs` owns completeness and every hex by comparing the parsed document against a
whole-struct `PaletteDoc` literal. `tests/skin.rs::TOKENS` is the checked-in
`(design token, hex, ColorRole)` table, covering only fields carrying a token (`bg_panel_2`,
`line_dim`, `line_soft`, `accent_soft` have no row); its length assert pins the written list against
the table, not the struct, so a new field takes a token row by review, never by assert.

`SkinDoc.menu` owns the four menu icon sizes. Menu typography resolves through `SkinDoc.text`, one
entry per type spec (family, weight, size, letter-spacing, default colour), so a tone differing from
the default is named on the node and mints no second entry. A `Dim` is a literal with no role
indirection: a `.kmodule.ron` cannot name a skin metric, and menu row heights live in the markup.
A document-declared frame is a `Canvas` over the container's plain background fill, drawing a
hairline on chosen sides in a colour of its own; neither carries a shadow, so a document node cannot
cast one. The pop-over needs both: `Anchored` draws its own background, frame, gold cap and shadow,
and `SkinDoc.pop.frame` has radius `0.0`, so all three share one square outline.

`SkinDoc` carries no words. The crossfader captions and the track list column and footer captions
are catalog entries resolved onto `Skin` alone; see "Text Catalog Ownership" for where they come
from.

## Text Catalog Ownership

`TextDoc` is the fourth `DocKind` (`kithara.text`), parsed by `parse_text` and owned by `kithara-ui`
the way the skin is: `builtin::text_doc()` is the compile-time asset, and its `LazyLock` is a
sanctioned panic site for an invalid embedded catalog, same as `builtin::skin_doc`. `compile` takes
`text: &TextDoc` borrowed for the call, right beside `skin: &SkinDoc` - a resource document, not
compile configuration, so it does not live on `UiConfig`.

A document literal beginning with `@` names a catalog key. `expand::binding_subst::intern_text`
resolves it between `substitute` and `interner.intern`, so `$`-substitution runs first and only the
substituted result is checked for the marker: an argument carrying `"@key"` resolves through the
catalog exactly like a literal `"@key"` written directly in the document. `@@` escapes to a literal
leading `@`, mirroring `$$`; a `@` anywhere but the first byte of the value is not a marker, so
`user@example.com` passes through unchanged. `Module.title`, `.chip` and `.assign` carry no
`$`-substitution, so they resolve a leading `@` through the same catalog lookup directly rather than
through `intern_text`, each against a synthetic `<prefix>/title`, `/chip` or `/assign/<index>` path.
An unresolved key is `UiDocError::UnknownTextKey { origin, key, path }`, a compile error, never a
rendered fallback - the same totality `UnresolvedParam` already holds for `$`.

Resolution happens once, at compile time, on document literals only. Text a host supplies through
`ReadValue::Text` never reaches `intern_text` and cannot become a key by starting with `@`; a track
title beginning with `@` stays a track title.

Two catalogs combine with `TextDoc::merge`, which unions their entries and fails with
`UiDocError::DuplicateTextKey` on any key present in both - never a silent override. `kithara-app`
merges `builtin::text_doc()` with its own small catalog (`assets/ui/app-en.ktext.ron`) once per
`compile_ui` call; the app catalog holds only the words canon has no key for (its own window-manager
menu - `Modules`, `Broadcast`, the two layout-count labels), so an app document reaches every other
key through the canon catalog even though the `.kmodule.ron` file naming it lives under
`kithara-app/assets`. `every_shipped_catalog_carries_the_same_key_set` in `tests/text.rs` is what
keeps a second-language catalog from silently dropping a key later.

`Skin::resolve` is the one place catalog resolution happens outside `compile`: it takes `text:
&TextDoc` and resolves the crossfader's three captions and the track list's ten column and footer
captions once, by fixed key, storing them on `Skin` as `CrossfaderLabels` and `TrackListLabels` (plus
`tree_search_placeholder`) rather than on `SkinDoc`. No control declares these keys and no document
names them; a `Crossfader`, `TrackList` or `Tree` control takes its captions from whichever catalog
`Skin::resolve` was given, unconditionally.

## Wave View Ownership

Hero-wave zoom and playback position are host-owned scalars. An optional `Wave.zoom` binding reads
the visible track fraction; a wheel detent emits `SetScalar` at `<wave-path>/zoom`, horizontal drag
emits `SetScalar` at the wave path for playback position. The renderer keeps neither value, derives
the centred window from each read snapshot, and falls back to `zoom_math::DEFAULT_ZOOM` when the
zoom read is absent.
`widgets/wave/zoom_math.rs` owns the zoom scale for every widget that draws a wave: opening window,
`MIN_ZOOM`/`MAX_ZOOM` clamp, and what each gesture is worth - a wheel detent (`1.25` / `0.8`) is
finer than a zoom button (`BUTTON_FACTOR = 0.7`). A host driving zoom from a button takes
`render::zoom_in` / `render::zoom_out` and gets the same bounds.
Bars tile the track from its origin, so a bar's content never depends on the playhead; the window
only selects which bars are visible and where they land, and playback scrolls instead of resampling.
Playhead dimming is per style: hero dims left of the playhead with `SkinDoc.wave.played_alpha`
mapped through its zoom window, bars dims the full track with `overview_played_alpha`, micro dims
nothing.

## Active Tone Ownership

`Text`, `Glyph` and `Row` each carry an optional `active` binding - one shape across three carriers.
It is a host-owned Bool read through `render/tree/read.rs::read_flag`, the one path every binding
read takes; an absent read means inactive. `Text` renders the content its `read` binding or `label`
supplies; `active` selects only a tone, never content.

- `render/tree/geometry.rs::active_tone` is the single selection rule: the active role when the node
  is active and declares one, otherwise the base role.
- `Text` and `Glyph` name their own `ColorRole` pair through `color` / `active_color`, the way
  `Row.background` selects among palette roles; a node naming no colour takes its skin entry's.
  `Glyph` sizing stays skin-owned through `GlyphStyle`, and `active_icon` switches the glyph itself,
  so one caret is one node with one path and one style.
- `widgets/text.rs::text_role` is the one `TextStyle` to `TextRoleSkin` join, feeding `active_tone`
  the node's pair with the skin entry's active colour behind it (`text.deck_letter_active`, marking
  the focused deck). No wildcard arm, so a new style must be given a skin entry.
- `Row` alone carries `active`, `active_background`, `frame_color`, `active_frame_color`; `Column`
  carries none. `geometry::frame_tone` resolves the frame pair, defaulting to the skin divider;
  `widgets/module/chrome.rs::frame_overlay` takes colour and width as arguments and reads no skin
  section, which lets one surface carry two hairline colours.

An `active` binding needs no `id`, and the shipped App Menu relies on that: `validate` requires an
id only for a container declaring `write`, and `expand::machine::container_bindings` addresses an
id-less container as its own module, its `ControlSite` path being the enclosing prefix shared by
every id-less sibling. That sharing holds only while the visitor body stays validation-only -
`check_controls` keys nothing by the path, and a container without `write` yields no `SurfaceSpec`.
A visitor keeping state per `ControlSite.path` needs the id rule widened first.
Joins live one place each: `text_role` in `widgets/text.rs`, `glyph_tone` in `render/tree/atom.rs`,
`frame_tone` beside `active_tone` in `render/tree/geometry.rs`, both called from
`render/tree/node.rs`. `render/tree/mod.rs` keeps `geometry` private and re-exports only
`active_tone`, so a join under `widgets` would duplicate the rule. Each is pinned by a
`#[cfg(test)]` module asserting both polarities against the skin field the style selects.

## Meter And Visualizer Ownership

`Meter` reads one Scalar, draws a horizontal fill and accepts no write, so the value stays
host-owned. `SkinDoc.meter` owns track size, hairline frame, track colour and fill colour; the fill
is inset by the frame width on every side, so a full bar stops at the frame's inner edge.
`Vis` is render-only and emits nothing. It reads the host-owned preset through its own binding as a
Scalar index, the master `player.output.levels` Stereo snapshot, and the animation clock as a Scalar
at `vis.time`. Reaction level is `max(l, r) * volume` clamped to `0..=1`; a non-finite level, an
out-of-range preset or any missing read collapses the widget to `Space`. It keeps no audio state and
no wall clock, so pacing belongs to the host. The embedded WGSL asset owns the three fixed presets
(`PRESET_COUNT = 3`), chrome around it is markup plus `SkinDoc.vis`, and the shader stays behind the
`render` feature so the non-render wasm schema lane needs neither wgpu nor a clock.

## Scoped Read Resolution

A read binding with a non-empty `with` map resolves through the canonical scoped key
`<endpoint>@<scope>=<value>[,<scope2>=<value2>...]`, scope names in `BTreeMap` order. The key is
built by `expand::scoped_key`, interned once at compile time, and carried by `Binding::key`
(`key == id` when the scope is empty). `render/tree/read.rs::resolve` passes exactly this key to
`Reads::get`; hosts key their read maps by the same form. A `Command` binding reads as `None`.
Widgets reading derived endpoints beyond their binding (`DeckSummary`, `Bpm`, `Time`, `MiniWave`)
take the read binding's scope suffix (`@deck=a` or empty) from `read::read_scope` and append it to
each derived endpoint, so `deck.track.title@deck=b` stays addressable per deck. `TrackList` column
state takes the suffix of the `columns_state` binding instead. Host-global endpoints
(`player.output.levels`, `ui.preset`, `vis.time`) stay unscoped.

## The Address Tree

`Reads::get` is the renderer's boundary and stays flat: one canonical key in, one value out.
`render::address` is how a host organises the answer behind it. A `Node` resolves one segment into a
child and reads its own value, knowing neither siblings nor parent, so no type carries the whole
vocabulary. `Walk` adapts a tree of them to `Reads`: split the key at `@`, walk the dotted path
segment by segment, read the leaf.

- The scope selects an instance rather than naming a path segment, and the node owning the instances
  is the one that spends it - a leaf reading a scope it does not own can answer for an instance that
  does not exist.
- Both `Node` methods default to `None`: a node answers only what it claims, and an address nobody
  claims reads as absent, which the renderer shows as its default.
- A node whose value borrows from data built for the frame implements `Node` for `&Self` so that
  data outlives the walk; a node borrowing only longer-lived state implements it by value.
- A module document id becomes a segment of `ui.module.<id>.collapsed`, so
  `validate::check_module_id` rejects a module id containing `.`.

## Typed Control Schema

Each supported control is a structural `ControlNode` enum variant. RON deserialization owns field
validation, so the document layer has no string control discriminator, property map, or property
kind catalog. Common control fields are repeated in the serde variants because RON flattening is not
part of the schema contract.
Captions attached to a control - the fader's inline label, the knob's caption under its dial - are
document text on the control node; the skin owns their typography, and the knob's caption is a full
text role down to family and letter-spacing. A control without a caption renders bare.
A waveform column is one bar wide for all three bands: low, mid and high draw from the vertical
centre over each other and nest by level, never by width, so a single `bar_width` and `bar_gap` set
the column pitch for both the deck wave and the overview row.

### Sizing

- Controls take their size from the wrapper, not from themselves: a widget fills what
  `size::control_size` or the document gives it, and pinning its own height would break its row.
- The declared size is the whole control box - the knob's caption row sits inside it and the dial is
  what remains, so a 28 px dial asks for a 39 px box and a square declaration renders a squashed
  dial. A knob declaring no size takes `skin.knob.size`, the one place those two numbers are kept
  together.
- A wave takes its box from its style: `Hero` from `skin.wave.size`, `Default` from
  `skin.wave.default_size`, `Micro` from `skin.wave.micro_size`. The three floors - `120`, `40` and
  `34` - are the heights the rows those styles stand in are built to, so the strip in a `42` bar
  asks for the room a `42` bar has. `size::control_size` matches all three, so a fourth style names
  its own number before it renders.
- A menu glyph is the one control whose declared size names one axis only: drawn as a text glyph,
  its box is the icon size wide and a line box tall (`1.3` times the size, the iced default), so a
  square declaration overflows. `size::icon_cell` fixes the width and leaves the height to the row.
- A container declaring no `size` renders `Fill` on both axes, mapped in
  `render/tree/geometry.rs::content_size`; a row that should hug its content says `h: Shrink`.
- `Dim::Shrink` is the one rule the document layer cannot compose: the toolkit measures the content,
  so `Bounds` treats it as an open axis and `Dim::from(Bounds)` never produces it. A shrunk node
  must carry `Shrink` to its own children - `content_size` passes it to the container, its frame
  overlay and its fill, because the first `Fill` inside a shrunk box claims the whole row.
- A transport cell carries its hairline on the sides `SkinDoc.button.transport_sides` names (only
  transport styles read that declaration); a `Button` declaring `frame` names them itself. One seam
  stands between neighbouring cells and none at the strip's ends, so cells before the flexible gap
  keep the skin's right seam and cells after it take a left one.

### Wheel surfaces

A `Row` or `Column` declaring `write` is an interactive surface over its whole box (`SurfaceSpec`,
drawn by `widgets/wheel.rs::WheelSurface`):

- A wheel detent emits one signed `ControlAction::StepScalar`, a double click emits
  `ControlAction::Activate`, both on the container's own path.
- A trackpad gesture steps by the sign of each pixel delta, no sooner than
  `WheelState::STEP_INTERVAL_MS` (200 ms) after the last step; `WheelScrolled` carries no scroll
  phase, so that interval is what bounds macOS momentum deltas after the fingers lift.
- A held press drags the same step at `WheelState::DRAG_STEPS_PER_PIXEL` (0.25) per pixel, upward
  for a step up, measured from where the last step left off so the surface never needs the value.
  The press consults the double click first, so the second press of a pair resets rather than drags.
- The surface counts steps and nothing else: it reads no value and holds no range, so what a step is
  worth and what the click returns to belong to the host owning the parameter.
- It claims the pointer over its whole box, reporting `ResizingVertically`, which in a `Stack`
  levitates the cursor away from everything it covers.
- Such a container needs an `id`; `validate::check_module_node_ids` otherwise rejects the document
  with `UnaddressedSurface`. A container silent about `write` carries no write path.

`validate::value_kinds` is the single owner of control read/write endpoint kinds. Intrinsic sizes are
selected exhaustively from `ControlSpec` and the supplied `SkinDoc` by `size::control_size`,
available in non-render and wasm builds. Renderers match `ControlSpec` directly and resolve no
runtime control catalog.

## Markup Composition

Cross-axis alignment is the container's: a `Row` centres its children, a `Column` leads them. Every
document in this crate, in `examples/gallery/assets` and in `crates/kithara-app/assets` takes those
defaults; a column wanting centred children composes it with spacers.

A `Row` distributes no main-axis alignment, so a fixed-size cell centres its content by composition:
the content sits between two spacers that are `Fill` on both axes, carrying no id so no spec table
names them. A leading indent spacer composes with its row's own `gap`, inserted after the spacer as
well as between every other pair, so a pinned indent is spacer plus row padding plus that gap. A
trailing spacer belongs to the container whose bottom padding it reproduces, as its last child.
Checked-in spec tables pin each node's own declared numbers and never a child's resolved position,
so no row of theirs can assert a composed offset.

## Module Chrome And Collapse Ownership

`ModuleDoc` owns optional shell labels, static assign labels and footer binding plus a typed
`ChromeStyle`. `Frame` is the serde default and renders the plain module frame, `Plain` renders only
module content, `Full` adds skin-owned header, separators and footer with assign labels as header
chips immediately before its chevron.
Each layout module instance owns which outer frame sides render (`FrameSides`, all four true by
default) and whether decorative corner ticks draw at its top-left and bottom-right (opt-in), letting
adjacent modules yield shared edges to the layout grid. The skin owns frame thickness and colour and
corner tick size, width, offset and colour.

A layout declaring `dragged` names what the pointer is carrying: while that binding reads as
non-empty text the renderer draws it at the pointer, over everything the layout lays out. The ghost
paints only - no event captured, no cursor claimed - and asks for a redraw as the pointer moves, so
following the pointer costs the host no messages. `SkinDoc.drag` owns its box and type; box position
relative to the pointer and label elision (against a mono advance estimate, since a canvas cannot
measure text) belong to the widget.

A module declaring `drop` takes dragged items: it emits
`ControlAction::Drag(DragPhase::Over(bool))` on `<instance>/drop` as the pointer crosses its bounds
and outlines itself while its `read` binding is true; `write` names the command the host runs on a
drop. The renderer never learns what is being dragged - the drag source reports its own start and
release on its own path, and the host holds the item and decides what a drop means.
Collapse state is host-owned. A Full module reads `Bool` from `ui.module.<module-doc-id>.collapsed`
(absent means expanded) and header activation emits `UiEvent::ToggleModule(<module-doc-id>)`. The
renderer neither retains nor mutates collapse state; Frame and Plain modules ignore that endpoint.

## Optional Block Ownership

`Optional` wraps exactly one node in a module or layout tree and marks it a block the host may hide.
It is a wrapper, not a field on `Row`, `Column`, `Include` or `Module`: those own layout, and
optionality is a separate responsibility. Visibility is host-owned: the document names the endpoint
through `hidden` and kithara-ui invents none, the binding reads as `Bool` exactly like `Text.active`
and `ModuleDrop.read`, and an absent read means visible. `validate::value_kinds` owns that kind for
a module-level wrapper, `validate::check_layout_block` the same kind at the layout level. The
binding takes `$`-substitution and scoped-key resolution like every other, so
`with: { "deck": "$deck" }` gives per-deck visibility without a crate-level scoping rule.

A block is hidden by its parent skipping it while iterating children, so it may only sit where some
parent iterates: a `Split` child, or a `Row`, `Column` or `Slot` child. `validate` rejects it
anywhere else - layout root, module root, directly under another `Optional`, and under a `Popover`
anchor/content or a `Pressable`. The rule is total, so renderer and sizer need no case for a hidden
node handed to them directly.

A block that must withdraw a click target along with what it draws wraps the `Pressable`, not the
glyph inside it. The fixed cell holding the slot open stays outside the block, a plain container
with its own id, so hiding the block moves no neighbour.
A block address names read state, so neither `.` nor `@` may appear in it, and
`validate::check_block_path` applies that rule to the whole expanded chain of enclosing ids - a
`mixer.a` module instance may not enclose a block. The id shares the one namespace its siblings use:
a module block id collides with a control id, a layout block id with a module instance. The compiled
block keeps the expanded path (`mixer/eq`, `deck-a/transport/eq`), addressable like a control.

A layout declares no parameters, so `expand::substitute` owns what `$` means at both levels:
`$name` in a layout-level `hidden` scope or in `Module.with` is a document error, `$$name` escapes a
literal `$name`, and inside a module the same function resolves `$name` against the instance's
arguments. An argument reaches a `String` field through `substitute`; an endpoint id through the
same call in `substitute_binding`, the substituted id being what the visitor hands
`validate::check_controls`; and a typed field through `param::Param<T>`, which reads either the
variant itself or a `"$name"` string. Serde tries the variant first, so a spelled-out one never
reads as a reference, at the cost that a misspelt variant parses as a reference - `resolve_param`
spends the missing `$` on `BadVariant` and an argument naming no variant on `BadParamVariant`, both
carrying the node path.

Sizing with blocks:

- A visible wrapper is fully transparent and delegates to its child, `content_size` and
  `effective_size` included, so it never reaches the undeclared-size mapping in `content_size`.
- A container decides emptiness over the children it actually lays out: a `Slot` whose only child is
  hidden has the `SizeSpec::FILL` of an empty one, a gap is charged between visible children only,
  and an all-hidden `Split` folds to `Dim::Fixed(0.0)`.
- `size::BlockNode` is the single owner of "which block does this node declare", implemented once
  for `ExpandedNode` and once for `CompiledNode`, so `size::visible` filters both stages through one
  predicate built from `read::read_flag` - which makes a `Command` endpoint resolve to nothing.
- A subtree's intrinsic size is a function of the node and the host snapshot, constant when neither
  an optional block nor an adaptive node sits below it. `CompiledNode` records that at compile time
  in `blocks`, which `size::has_blocks` sets, and `render/tree/size.rs::node_size` returns the
  precomputed `compile::compiled_node_size` for such subtrees instead of walking them once a frame -
  memoization, not a fallback path. A layout `Module` declaring its own `size` records
  `blocks: false` and never re-walks.
- `size::Snapshot` is the single description of what the host answers about a tree, with one method
  per question the sizer asks: which blocks are hidden, and what each adaptive measure reads.
  `size::DEFAULTS` is the state before any answer - every block visible, every measure silent - and
  `CompiledUi.size` is the size function evaluated in it. `render/tree/read.rs::Answers` is the one
  implementation that reads a host, so snapshot-aware sizing stays toolkit-independent and available
  in non-render and wasm builds.

## Adaptive Branch Ownership

`Adaptive` declares one place in several forms and draws exactly one of them: `base`, plus `steps`
ordered by the `from` threshold each takes effect at. The selected branch is the last step whose
`from` the measured value reaches, and `base` below all of them - a step function over the whole
real line, not a chain of attempts. `expand::adaptive_branch` is that function, pure in the
thresholds and the value alone.

- `measure` reads `ValueKind::Scalar` (`validate::value_kinds`), and the whole read is total in one
  place: `render/tree/read.rs::read_measure` answers `None` for a missing read, a value of another
  kind, and anything the `f64` to `f32` cast leaves non-finite. `None` is `base` by contract, the
  same rule that makes an unread `Optional` visible and an unread `Popover` closed.
- Thresholds are document literals in whatever unit the endpoint measures, and
  `validate::check_adaptive_steps` requires at least one step, each starting at a finite value above
  the one below it. Order is what makes the
  selection total without a tie-break, so a document that breaks it is rejected rather than resolved.
- The node contributes no segment to the addresses below it: `expand::machine::expand_adaptive`
  spends its own `id` on the visitor and its binding, then walks every branch with the enclosing
  context. An author repeating an `id` across branches gets one address and one host handler, and
  that is the point - `validate::walk_branches` gives each branch a clone of the ids its parent
  holds and unions the claims on the way out, so ids collide inside a branch and never across them.
- A node reading its measure declares no `size` and measures as the branch its snapshot selects, in
  `size::compute_size` and in `geometry.rs::effective_size` alike. Both take the snapshot for that
  reason, and a wrapper above the node reports the drawn branch's size the way it already reports
  an `Optional` child's - `Pressable` over an adaptive bank keeps the box the bank declares. The
  render arm returns the branch's own element, the way `Optional` returns its child's. It always
  yields one child, so it needs no iterating parent and stands wherever a plain node stands. Every
  branch expands at compile time, so the arena and the node budget carry them all.
- A node measuring `Width` or `Height` instead reads the box the toolkit gives it, and so must
  declare that axis: `validate::check_measured_box` rejects an absent `size` and a `Dim::Shrink` on
  the measured axis with `UiDocError::UnmeasuredAxis`, and a read measure declaring a box at all
  with `MeasuredBoxWithoutAxis`. Such a node answers its declared box whatever branch it draws,
  which is what keeps the pick from moving the siblings whose room decided it, and what lets
  `size::has_blocks` call the subtree constant. `widgets/adaptive/measured.rs` builds every branch
  once and lays out the one that fits, so a pick costs no rebuild and each branch keeps its state.
- The measured number belongs to the host endpoint, not to the node: a window width, a band count,
  anything else reads the same. Two axes are two nested adaptive nodes.

## Threshold Reveal Ownership

A `Row` or `Column` declaring `measure` reads the box it is given on that axis, and each child
wrapped in `Reveal { from }` stands once the axis reaches `from`. Several children stand at once -
as many as have thresholds the room reaches - which is where this differs from `Adaptive`, drawing
one branch of several. A child carrying no wrapper always stands.

- The container obeys the same declared-box rule a self-measured `Adaptive` does, through the same
  `validate::check_measured_box` and the same `UiDocError::UnmeasuredAxis`: `measure` requires
  `size`, and the measured axis may not be `Dim::Shrink`. So the container answers its declared box
  however many children it shows, `size::compute_size` and `geometry::effective_size` return that
  box, and `size::has_blocks` calls it constant - the cells appearing move no sibling, and the
  renderer memoises the subtree.
- That box is the answer while every ancestor leaves the axis free. An ancestor asking for an
  intrinsic size sets `Limits::compression` on that axis, `iced`'s `Limits::resolve` then takes the
  intrinsic size over `Fill`, and the container answers the room its shown cells take while the
  threshold still reads the maximum it was handed. Both widgets under `widgets/adaptive` size
  themselves through that call, and validation lets such an ancestor through, so a document that
  puts one there gets a box that moves with its cells inside a subtree `has_blocks` calls constant.
- Only that container answers a threshold, so a `Reveal` stands only among its direct children;
  anywhere else is `UiDocError::UnmeasuredReveal`, never a child that silently stands for good.
  `validate::Sibling::Measured` is what a container declaring `measure` passes its children, and
  `Sibling::Only` is what a `Reveal` passes its own, so an `Optional` directly below one - which no
  parent would iterate, and so no parent would hide - is rejected with the rest.
- A threshold is finite and not negative (`UiDocError::RevealThreshold`) and that is the whole rule:
  thresholds carry no order among themselves, unlike `Adaptive` steps, whose order is what makes one
  branch total.
- The node carries no `id` and contributes no segment to the addresses below it: it publishes
  nothing and its visibility is the room's answer rather than the host's, so a control inside one is
  addressed exactly as a control beside it.
- Hiding and revealing compose without meeting: `render/tree/node.rs::revealed` filters the
  host-hidden children through `size::visible` first, so the widget lays out only children the host
  keeps, and an `Optional` may stand among `Reveal`s as a plain always-revealed cell.
- The threshold is read against the box the container declares, padding included:
  `widgets/adaptive/revealed.rs` owns `pad`/`pad_x`/`pad_y` and charges them inside itself, so the
  number a document names is the number a parent hands the container.
- For the same reason a measuring `Column` is handed both axes of its declared box, where a plain
  one leaves its height to its content: the axis it measures has to be readable from the box, and a
  `Column` measuring `Height` declares one.

`widgets/adaptive` holds both self-measuring widgets. `Revealed` lays the shown children out
through `iced`'s own `layout::flex::resolve`, so `gap`, padding, cross-axis alignment and `Fill`
distribution are the toolkit's rules rather than a second set. That resolver takes one contiguous
slice, so the pass rotates the shown children to the front of both the element list and the state
list, resolves, and rotates them back before it returns;
widget state is bound to position, and `Tree::diff_children` must find the order the element tree
was built in. A child left out is given a layout node of no size, and `draw`, `update`,
`mouse_interaction` and `overlay` address the shown children alone - the pass records them in the
widget state - so a hidden cell holding an open `Popover` floats nothing.
Neither widget forwards `Widget::operate` to its children, so an operation traversal - focus,
programmatic scroll-to - stops at the container and reaches nothing inside it.

## Minimum Room Ownership

A tree answers two questions. `size::compute_size` answers what box a node shows its parent, where a
declared `size` replaces everything below it - that constancy is what lets `size::has_blocks` call a
self-measured subtree memoisable. `size::min_size` answers how much room the node actually needs, and
there a declared box is a floor rather than the answer: the result is the per-axis maximum of the
declared minimum and what the children compose to, so a box cannot swallow the room its cells take.
`min_size` takes no `Snapshot`: it is a property of the document and the skin alone.

- A `Control` needs what `compute_size` answers for it, a declared box included - it holds no cells
  whose room a box could swallow. `Popover` needs its anchor, `Pressable` needs its child, and
  `Scroll` needs its declared box when it has one, since its content is what scrolls inside it.
- An `Optional` child counts as standing. The host owns that switch, and a bar that overflows the
  moment the host shows a block again is a bar that never fitted. A `Reveal` child is the opposite
  case - the room owns it - so a cell waiting for a threshold contributes nothing until the room
  reaches it, on both axes.
- An `Adaptive` needs its base branch: that is the form it draws in the smallest window. What its
  steps need is a separate question, answered by the rule below rather than by the node's own
  minimum.
- Which cells a measuring container stands at its own minimum depends on that minimum, so
  `size::Stack::settled` resolves the circle by climbing: the room starts at what the cells waiting
  for nothing take, and each round admits the cells whose threshold that room reaches. The climb is
  monotone and thresholds are finite in number, so one round per waiting cell reaches the least fixed
  point; the loop is bounded by that count and answers whatever it settled on, since a further round
  on a settled set repeats it. Each child's minimum is taken once, before the rounds, so nested
  measuring containers cost one walk rather than one per round.
  A container reading no room stands every cell, which is what `render/tree/node.rs::render_node`
  draws for a `Reveal` outside a measuring container.
- `compile::compiled_min` carries the same question up the layout tree - `Split` composes, `Optional`
  takes its child, `Adaptive` takes its base, a `Module` takes its expanded root plus its chrome -
  and `CompiledUi::min` is the answer at the root. That is the number a host holds its window to, and
  `compiled_min` is public so a host can ask the same of one branch it stands behind a threshold. A
  module's minimum maxes against the box its `CompiledNode` carries, which is the box the layout
  declared or, absent one, the composed size that box would have had - below the minimum either way.

Two rules follow, both checked at compile time against the skin, because both are questions a
document can only answer once its controls have sizes.

- A step draws in the room its threshold promises: `room::check_steps` requires
  `step.from >= min_size(step)` on the measured axis, for `ExpandedNode::Adaptive` reading `Width` or
  `Height` and for every `CompiledNode::Adaptive`, which reads one of them by construction. A
  `MeasureSpec::Read` step is left alone: its threshold counts whatever its endpoint counts - bands,
  decks, anything - and pixels are not that unit. Failure is `UiDocError::AdaptiveStepRoom`, naming
  the step, the threshold and the room the branch needs.
- A threshold names room the container has: `room::check_cells` requires that the cells standing in a
  room fit in it, at the container's own minimum and at every threshold above it. The standing set
  stands still between those points, so they cover every width rather than sample it. A threshold
  below the minimum names a cell that always stands, which the minimum already counts, and is no
  error. Failure is `UiDocError::RevealRoom`, naming the container, the room and what is needed
  there.

## Icon Identity

A document names an `IconName`; `render/tree/icon.rs::render_icon` joins it to `render::Icon`
(`render_tree_icon` joins the host-facing `TreeIcon` the same way), and `render/icons.rs::source`
joins `Icon` to a lucide glyph or an embedded SVG. The legs are guarded differently: coverage by an
exhaustive match with no wildcard, so a new `IconName` does not build until given an arm; which
glyph an arm names by a runtime table in that file's test module, compared by codepoint because
`lucide_icons::Icon` has no equality.
Role-named variants are the drift surface: `Faders`, `Collection`, `Charts` and `Waveform` are named
after their purpose, and each has a lucide-named neighbour that looks like a plausible home. The
guard asserts both directions - each menu glyph resolves to its namesake, and no role-named
incumbent resolves to the neighbour it could be mistaken for. New variants take the lucide glyph
name in UpperCamel.

## Container Press Ownership

`Pressable` makes any subtree a click target: `ControlAction::Activate` on a left press,
`ControlAction::SecondaryActivate` on a right press, both on its own path and inside the existing
`UiEvent::Control`, so a right-clicked node is addressed exactly as a left-clicked one.

It is a wrapper rather than a `press` field on `Row` and `Column` because `validate::value_kinds`,
the single owner of read and write endpoint kinds, answers per variant: a `press` field would make
`Row` answer two write kinds depending on which field is populated. As a variant, `Pressable`
answers `(None, Some(Trigger))` and rides the existing write site unchanged, while `Row`/`Column`
`write` keeps its wheel meaning (`Some(Scalar)`).

The innermost target wins with no hit-testing: `mouse_area` forwards the event to its content first
and returns as soon as the shell reports it captured. That capture is pass-global -
`Shell::capture_event` sets one sticky flag every sibling in the pass shares - so a `Pressable` is
suppressed by anything that captured earlier in the same traversal, not only by its own descendants.
Harmless beside presses gated on the cursor being over their bounds; not harmless beside a widget
that captures away from the cursor, such as a `WheelSurface` or a track-list row mid-drag.
A `Button` inside a `Pressable` resolves by phase, not by nesting: `button` captures the press and
publishes on the release, `mouse_area` publishes and captures on the press. The press is the
button's and the `Pressable` stays silent; the release is the button's and the `Pressable` has no
release to fire.

## Popover Ownership

`Popover` floats one subtree over the layout while a host-owned Bool reads true; an absent read
means closed - the default-state shape `Optional` and collapse already carry. Only `anchor` is laid
out in flow and it alone owns the node's intrinsic size (`size::compute_size` and
`geometry::effective_size` delegate to it); `content` is measured inside the overlay and contributes
nothing to the parent. The node carries no `size` field - the content column declares its own width
and the widget draws the chrome outward of it.
An optional block below a popover's `content` is charged, validated, interned and given a
`BlockSpec` like any other, yet `size::has_blocks` stops at the anchor, so the enclosing module
records `blocks: false` and answers from the memoized value. Content contributes no size in any
snapshot, so this is the one place where "a block sits below" and "the size can change with the
snapshot" diverge.
A popover must not open inside another popover's content, enforced during expansion
(`Expander::in_popover`) rather than in the per-document node walk: expansion is the only stage that
sees across `Include` boundaries, and a `Popover` is legal at a module root, so a document-local
rule would let `Popover(content: Include(m))` through whenever `m` declares one.

The document names which geometry the surface opens from and which of its edges lines up with it;
the widget owns how. `at: PopoverAt` chooses anchor rectangle or pointer, defaulting to `Anchor`;
`align: PopoverAlign` picks the edge, defaulting to `Start`, `End` lining the surface's right edge
up with the anchor's. A menu on a full-width row needs `Pointer` (an anchor rectangle spanning the
list cannot say where the user clicked); a burger under a fixed cell needs `Anchor`.

Everything else stays in `widgets/anchored.rs`:

- `place` puts the surface below whichever geometry it opens from, overhangs it `FRAME_OVERHANG`
  (1 px) past the aligned edge so the content column lands flush, flips above when the room below
  runs out, and clamps both axes into the viewport; a surface taller than the viewport starts at the
  top and overflows downward.
- A `Pointer` popover opens at the press that opened it: `Anchored` records `Cursor::position` on
  every `ButtonPressed`, `latch` consumes that record on the false-to-true edge of the open flag (no
  banked press places at the anchor), and `Widget::overlay` carries the latched point through the
  same translation as the anchor rectangle. The flip frame cannot read the live cursor -
  `iced_runtime` hands the base tree `Cursor::Unavailable` while the overlay claims any interaction.
- The surface claims the cursor over itself and nowhere else: claiming wider would cost the whole
  base tree its cursor for that pass, the signal deciding whether the pointer belongs to the overlay.
- `SkinDoc.pop` solely owns the pop chrome - background, frame, gold cap, shadow - and `Anchored`
  paints all four; markup declares only content. Frame and cap draw outward of the content column,
  so a 298 px column yields a 300 px surface exceeding the content height by twice the frame plus
  the cap.
- `Anchored` holds exactly two children: `render/tree/node.rs` hands it `iced::widget::Space` for a
  closed popover's content rather than dropping the child, so the two-entry split in
  `Widget::overlay` and the single layout child in the overlay's `draw` and `update` are total
  patterns, not fallback branches. `Tree::diff_children` rebuilds the content state on each open.
  The overlay's layout node is the whole surface with the content column as its single child, so
  `layout.bounds()` means "the popover" everywhere and a press on the frame belongs to the menu.
- `Widget::overlay` always wraps in `overlay::Group`, including a group of one: without it the layer
  is bounded by the bare `Anchored` element, which is exactly the surface, and the shadow drawn
  below and blurred outward falls outside it. `Anchored` declares no overlay index and is the
  crate's only overlay-producing widget.
- Dismissal fires only on a mouse press with the cursor outside the surface, or on Escape - never on
  a release, a move or a scroll, since the opening press is captured while the popover is still
  closed and its release lands on the fresh overlay over the anchor. The widget publishes
  `ControlAction::Activate` on the `Popover`'s own path, never the anchor's, and captures the event
  so the anchor's press cannot also fire; the host's popover handler is therefore set-false and
  never a toggle, the anchor's press stays the only toggle, and outside press and Escape are
  idempotent.

## Shipped App Menu

`assets/modules/app-menu.kmodule.ron` and the row templates it includes from
`assets/modules/app-menu/` are shipped assets that `builtin::resolver()` deliberately does not
answer for: their window-manager endpoints - window list, per-window module flags, saved layouts -
are host state no crate owns, so the documents must not become canonical preset surface the app
can resolve. Exactly one copy of each exists and every consumer reaches it with `include_str!`.
The window row, module-grid cell, saved-layout row, preference toggle and hint-reporting toggle are
one template each (`window-row`, `module-cell`, `layout-row`, `toggle-row`, `hint-row`), taken as
often as the menu needs through `Include`. Each instance's control paths are
`app-menu/<include id>/<node id>`, so the template's own ids stay plain.

## Clock And Pivot Ownership

`assets/modules/master-clock.kmodule.ron`, the deck key-lock and overview row, and
`assets/modules/pivot-portals.kmodule.ron` are shipped component documents over host-owned timing
state. They are not builtin app presets: the Gallery embeds them explicitly and supplies mock
endpoints, while a production host must supply its measured clock sources, transport state and
portal policy. The documents retain no tempo, source, range, Link or MIDI state.

`PortalMap` and `Range` are the dedicated renderer primitives in this group. `PortalMap` copies one
`PortalMapView` snapshot for the iced canvas lifetime and draws the declared master, range and
target arcs with `SkinDoc.portal_map` geometry. `Range` reads normalized bounds and emits a scalar
at `<control-path>/min` or `<control-path>/max`; the host owns snapping and the minimum gap. The
generic `Scroll` document container provides the bounded portal table. Selection, portal
enumeration and timing decisions remain host-owned.

## Stream Quality Ownership

`assets/modules/deck/quality.kmodule.ron` is the deck's stream-quality cell and its menu, taken per
deck through `parameters: ["deck"]`. It is markup over `Popover`, `Pressable` and the row templates
in `assets/modules/deck/quality/`, so the crate gains no control for it; its surface is the crate's
own `Pop` chrome, which is where it departs from the design's `HlsCell`. The transport bar holds it
before `TEMPO` as an `Optional` block: a ladder belongs to the stream, so a deck on a plain file
answers `deck.stream.quality_hidden` and the cell leaves the row with its seam.
The ladder is host-owned; the document holds six slots (`variant` `"0"`..`"5"`), each an `Optional`
the host hides through `deck.stream.variant_hidden`. `quality/row.kmodule.ron` names
`deck.stream.variant_label`, `variant_sub` and `variant_active` under the `deck` and `variant`
scopes, and writes `deck.stream.select_variant`. Automatic selection is a sibling template,
`quality/auto.kmodule.ron`, on the same endpoints with `variant: "auto"` pinned, so the host owns
what automatic means. The cell reads `deck.stream.quality` for what it shows and
`deck.stream.quality_menu` for both its open state and its two-state fill;
`deck.stream.toggle_quality_menu` is its only toggle, and the popover's own path carries the
set-false the widget publishes on dismissal.

## Window Chrome Ownership

A layout setting `resize_edges` is framed by `render::tree::render` with the eight drag zones a
system border would have given it. They lie over the content, not beside it, so declaring them costs
the layout no space; `SkinDoc.window.resize_edge` owns their thickness, and the host maps each
`WindowEdge` to its toolkit's resize direction.
`WindowDrag`, `TitleBar` and `WindowControls` paint no surface of their own and take their size from
the row that holds them, so the same controls sit in a 26 px gallery header and in the app's
42 px bar; the document declares the background. `WindowDrag` is the bare drag surface a bar without
a title needs, `TitleBar` the same surface with a label. Both emit on press, not on release - a
window drag only takes effect while the button is still held. Their glyphs are canvas strokes drawn
to the skin's icon size.

`TitleBar` and `WindowControls` are portable, binding-free controls emitting typed
`UiEvent::Window(WindowCommand)` values; the host owning the native window ID executes drag,
minimize, maximize and close. `kithara-ui` owns their declarative schema and skin-driven
presentation, never native window state.

## Track List Column Ownership

`TrackList` owns an ordered typed `Vec<TrackColumn>` and requires `Title` during compilation. The
renderer owns table geometry and cell presentation but not column visibility: with a `columns_state`
binding present the host may expose Bool reads at `<binding-id>.<column endpoint_name>`, and a
missing derived endpoint means that column is visible. This keeps one declarative column inventory
while letting library, playlist and set-queue hosts apply presets without renderer-owned state.

The `Deck` column marks assignment and does not offer it: it shows the letters the host put on the
row, and nothing when there are none. A row is a drag source instead - pulling past
`behavior::ItemDrag::DRAG_THRESHOLD` (4 px) emits `ControlAction::Drag(DragPhase::Start(index))` on
the control path, and the release emits `DragPhase::Drop`. The gesture captures no event, so the row
keeps its own click and whatever the drag is released over sees the same release.
Column widths are host-owned through Scalar reads at `<binding-id>.width.<column endpoint_name>` and
`SetScalar` controls emitted at `<track-list-path>/width/<column endpoint_name>`; a missing width
read uses the skin default. The renderer retains only canvas drag state, clamps resizable fixed
columns to the skin minimum, and keeps the required Title column flexible with its skin-owned
minimum.

## Browser Tree Ownership

`Tree` reads a borrowed flat row slice whose depth, branch state, selection and presentation flags
are host-owned. The renderer never mutates or filters that state; activating any visible row emits
`ControlAction::SelectIndex` on the control path, and the host decides whether that index toggles a
branch or selects a leaf. `TreeSkin` owns search, row, indentation, panel and context-bar metrics.
`ContextBar` keeps breadcrumb text read-only; optional scope items use a separate Scalar read
binding and emit `SelectIndex` on the control path so scope state stays host-owned.
`validate::check_context_scope` requires `scope_items`, the scope read and the write to appear
together or not at all.

## Application Consumer

The `kithara-app` GUI is the production consumer: it embeds its own layout and module
documents, implements `EndpointRegistry` (`gui/ui/endpoints.rs`) and `Reads` over an address
tree (`gui/reads/`), and maps `UiEvent` to app messages (`gui/ui/events.rs`). Builtin
module docs under `assets/modules/` remain the canonical presets consumed by the gallery modules
page.
