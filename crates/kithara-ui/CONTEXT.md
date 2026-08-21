# kithara-ui - Context

Contracts and invariants for the kithara-ui crate; the README stays the overview.

## Compiled String Ownership

Every string the compiled tree retains is interned in one bounded arena owned by `CompiledUi`;
`UiConfig.max_arena_bytes` caps it, and the cap or a failed `try_reserve` returns
`UiDocError::ArenaFull`. `InternId` is valid only within the `CompiledUi` that produced it - never
persist one in application messages or state, host-facing paths stay owned `String`s.
`StrArena::resolve` is total: an unknown ID or invalid span resolves to `""`. No kithara-bufpool
here: budget-charging `ensure_len` needs `Default + Clone`, which `ExpandedNode` cannot provide.

## Document And Compiled Layers

`BindingRef` and the typed `ControlNode` variants are the serde document inputs; `Binding` and
`ControlSpec` their compiled forms, with string payloads interned and style, format, tone and
boolean fields typed. A layer split, not a second source of domain truth: endpoint validation runs
on the typed document variant and the substituted binding before interning. Arena types live in
`ids.rs`. The `LazyLock` in `builtin::skin_doc` / `builtin::skin` is the sanctioned panic site for
an invalid embedded document or colour.

## Skin Ownership

`SkinDoc` owns every configurable rendering metric, including the intrinsic control sizes the
toolkit-independent compiler reads; with `render`, `Skin::resolve` converts it to iced colours and
keeps the document behind `Skin::document()`. The platform monospace family stays code-owned in
`render/fonts.rs` - font availability, not skin design. `Skin::resolve` also copies every sub-skin
flat onto `Skin` for zero-indirection render access while retaining the nested document, so each
field's value lives twice; that duplication is the design, which is why `render/skin.rs` is in
`field_passthrough.exempt_files` in `.config/arch/thresholds.toml`.

The palette is the single colour vocabulary: a skin section names a `ColorRole`, never a hex, and
only `PaletteDoc::validate` parses one. Alpha stays a skin field beside the role it applies to. A
`ColorRole` is an alias for a value, and several role names do not carry the design token they sound
like - select a role by the value it holds:

| role | hex | design token |
| --- | --- | --- |
| `BgFooter` | `#1b1b32` | `panel2` |
| `BgSelect` | `#26264a` | `select` |
| `BgPanel2` | `#26264a` | none - duplicate of `BgSelect` |
| `LineInner` | `#2a2a4c` | `lineDim` |
| `LineSoft` | `#2a2a4c` | none - duplicate of `LineInner` |
| `LineDim` | `#242442` | none |
| `AccentSoft` | `#bb94422e` | none - and no consumer |

Two pins guard it: `palette_holds_exactly_the_declared_roles` in `doc/skin/document.rs` owns
completeness and every hex against a whole-struct literal, and `tests/skin.rs::TOKENS` is the
checked-in `(design token, hex, ColorRole)` table over the fields that carry one, its length assert
pinned against the table rather than the struct, so a new field takes a token row by review.

Menu typography resolves through `SkinDoc.text`, one entry per type spec, so a tone differing from
the default is named on the node and mints no second entry. A `Dim` is a literal with no role
indirection: a `.kmodule.ron` cannot name a skin metric. A document-declared frame is a `Canvas`
drawing a hairline on chosen sides and carries no shadow, so a document node cannot cast one, while
`Anchored` draws its own background, frame, gold cap and shadow with `SkinDoc.pop.frame` at radius
`0.0` so all three share one outline. `SkinDoc` carries no words: the crossfader and track list
captions are catalog entries resolved onto `Skin` alone.

## Text Catalog Ownership

`TextDoc` is the fourth `DocKind` (`kithara.text`), parsed by `parse_text`; `builtin::text_doc()` is
the compile-time asset with the same sanctioned `LazyLock` panic site as the skin. `compile` borrows
`text: &TextDoc` beside `skin: &SkinDoc` - a resource document, not compile configuration, so it
does not live on `UiConfig`.

A document literal beginning with `@` names a catalog key, resolved by
`expand::binding_subst::intern_text` after `substitute`, so an argument carrying `"@key"` resolves
exactly like a literal one. `@@` escapes a leading `@`, mirroring `$$`; a `@` anywhere but the first
byte is not a marker. `Module.title`, `.chip` and `.assign` carry no `$`-substitution and resolve a
leading `@` directly, each against a synthetic path. An unresolved key is
`UiDocError::UnknownTextKey`, a compile error, never a rendered fallback. Resolution happens once,
at compile time, on document literals only: text a host supplies through `ReadValue::Text` cannot
become a key by starting with `@`. `TextDoc::merge` unions two catalogs and fails with
`UiDocError::DuplicateTextKey` on any shared key, and
`every_shipped_catalog_carries_the_same_key_set` in `tests/text.rs` keeps a second-language catalog
from dropping one.

`Skin::resolve` is the one place catalog resolution happens outside `compile`: it resolves the
crossfader and track list captions by fixed key onto `Skin` (`CrossfaderLabels`, `TrackListLabels`,
`tree_search_placeholder`). No control declares those keys, so a `Crossfader`, `TrackList` or `Tree`
takes them from whichever catalog `Skin::resolve` was given.

## Wave View Ownership

Hero-wave zoom and playback position are host-owned scalars. An optional `Wave.zoom` binding reads
the visible track fraction; a wheel detent emits `SetScalar` at `<wave-path>/zoom`, horizontal drag
emits `SetScalar` at the wave path for position. The renderer keeps neither value and falls back to
`zoom_math::DEFAULT_ZOOM` with no zoom read. `widgets/wave/zoom_math.rs` owns the scale for every
widget that draws a wave - opening window, `MIN_ZOOM`/`MAX_ZOOM` clamp, what each gesture is worth -
and a host driving zoom from a button takes `render::zoom_in` / `render::zoom_out` for the same
bounds. Bars tile the track from its origin, so a bar's content
never depends on the playhead and playback scrolls instead of resampling. Playhead dimming is per
style: hero dims left of the playhead with `SkinDoc.wave.played_alpha` mapped through its zoom
window, bars dims the full track with `overview_played_alpha`, micro dims nothing. The micro style
alone carries a playhead tab (`skin.wave.playhead_marker_*`) and a bottom strip of
`skin.wave.cache_strip_height` running to `deck.playback.cached_normalized`, held to the playhead,
so a host answering nothing draws the played part alone.

## Active Tone Ownership

`Text`, `Glyph` and `Row` each carry an optional `active` binding - one shape across three carriers.
It is a host-owned Bool read through `render/tree/read.rs::read_flag`; an absent read means
inactive, and `active` selects a tone, never content.

- `render/tree/geometry.rs::active_tone` is the single selection rule: the active role when the node
  is active and declares one, otherwise the base role.
- `Text` and `Glyph` name their own `ColorRole` pair through `color` / `active_color`; a node naming
  no colour takes its skin entry's. `Glyph` sizing stays skin-owned through `GlyphStyle` and `active_icon` switches the
  glyph itself, so one caret is one node with one path and one style.
- `widgets/text.rs::text_role` is the one `TextStyle` to `TextRoleSkin` join, with no wildcard arm,
  so a new style must be given a skin entry.
- `Row` alone carries `active`, `active_background`, `frame_color`, `active_frame_color`; `Column`
  carries none. `geometry::frame_tone` resolves the frame pair and defaults to the skin divider;
  `widgets/module/chrome.rs::frame_overlay` takes colour and width as arguments and
  reads no skin section, which lets one surface carry two hairline colours.
- Joins live one place each - `text_role`, `glyph_tone` (`render/tree/atom.rs`), `frame_tone` and
  `active_tone` (`render/tree/geometry.rs`) - all called from `render/tree/node.rs`, and
  `render/tree/mod.rs` keeps `geometry` private so a join under `widgets` would duplicate the rule.
  Each is pinned by a `#[cfg(test)]` module asserting both polarities.

An `active` binding needs no `id`: `validate` requires one only for a container declaring `write`,
and `expand::machine::container_bindings` addresses an id-less container as its own module, its
`ControlSite` path being the prefix shared by every id-less sibling. That sharing holds only while
the visitor body stays validation-only - a visitor keeping state per `ControlSite.path` needs the id
rule widened first.

## Meter And Visualizer Ownership

`Meter` reads one Scalar, draws a horizontal fill and accepts no write; `SkinDoc.meter` owns its
metrics and the fill is inset by the frame width on every side. `Vis` is render-only and emits
nothing: it reads its preset as a Scalar index, the master `player.output.levels` Stereo snapshot,
and the animation clock at `vis.time`. Reaction level is `max(l, r) * volume` clamped to `0..=1`; a
non-finite level, an out-of-range preset or any missing read collapses the widget to `Space`. It
keeps no audio state and no wall clock, so pacing belongs to the host. The embedded WGSL asset owns
the fixed presets (`PRESET_COUNT = 3`) and stays behind the `render` feature, so the non-render wasm schema lane needs
neither wgpu nor a clock.

## Scoped Read Resolution

A read binding with a non-empty `with` map resolves through the canonical scoped key
`<endpoint>@<scope>=<value>[,<scope2>=<value2>...]`, scope names in `BTreeMap` order. The key is
built by `expand::scoped_key`, interned once at compile time, and carried by `Binding::key`
(`key == id` when the scope is empty); `render/tree/read.rs::resolve` passes exactly this key to
`Reads::get`, and hosts key their read maps by the same form. A `Command` binding reads as `None`.
Widgets reading derived endpoints beyond their binding (`DeckSummary`, `Bpm`, `Time`, `MiniWave`)
take the read binding's scope suffix from `read::read_scope` and append it to each derived endpoint;
`TrackList` column state takes the suffix of the `columns_state` binding instead. Host-global
endpoints (`player.output.levels`, `ui.preset`, `vis.time`) stay unscoped.

## The Address Tree

`Reads::get` is the renderer's boundary and stays flat: one canonical key in, one value out.
`render::address` is how a host organises the answer behind it - a `Node` resolves one segment into
a child and reads its own value, knowing neither siblings nor parent, and `Walk` adapts a tree of
them to `Reads` by splitting the key at `@` and walking the dotted path.

- The scope selects an instance rather than a path segment, and the node owning the instances is the
  one that spends it - a leaf reading a scope it does not own can answer for an instance that does
  not exist.
- Both `Node` methods default to `None`: an address nobody claims reads as absent, which the
  renderer shows as its default.
- A node whose value borrows from data built for the frame implements `Node` for `&Self`; a node
  borrowing only longer-lived state implements it by value.
- A module document id becomes a segment of `ui.module.<id>.collapsed`, so
  `validate::check_module_id` rejects a module id containing `.`.

## Typed Control Schema

Each supported control is a structural `ControlNode` enum variant, so the document layer has no
string discriminator, property map or property kind catalog; RON deserialization owns field
validation, and common fields are repeated in the serde variants because RON flattening is not part
of the schema contract. `validate::value_kinds` is the single owner of control read/write endpoint
kinds; `size::control_size` selects intrinsic sizes exhaustively from `ControlSpec` and `SkinDoc`,
available in non-render and wasm builds. Renderers match `ControlSpec` directly and resolve no
runtime control catalog. A waveform column is one bar wide for all three bands, which nest by level
rather than by width, so one `bar_width` and `bar_gap` set the pitch for every wave style.

### Sizing

- Controls take their size from the wrapper: a widget fills what `size::control_size` or the
  document gives it, and pinning its own height would break its row.
- The declared size is the whole control box - the knob's caption row sits inside it and the dial is
  what remains, so a square declaration renders a squashed dial. A knob declaring no size takes
  `skin.knob.size`.
- A wave takes its box from its style (`skin.wave.size`, `default_size`, `micro_size`), each a
  height the rows that style stands in are built to; `size::control_size` matches all three, so a
  fourth style names its own number before it renders.
- A menu glyph is the one control whose declared size names one axis only: drawn as a text glyph its
  box is a line box tall (`1.3` times the size, the iced default), so `size::icon_cell` fixes the
  width and leaves the height to the row.
- A container declaring no `size` renders `Fill` on both axes (`geometry::content_size`); a row that
  should hug its content says `h: Shrink`.
- `Dim::Shrink` is the one rule the document layer cannot compose: `Bounds` treats it as an open
  axis and `Dim::from(Bounds)` never produces it, so a shrunk node must carry `Shrink` to its
  container, frame overlay and fill - the first `Fill` inside a shrunk box claims the whole row.
- A transport cell carries its hairline on the sides `SkinDoc.button.transport_sides` names (only
  transport styles read that declaration); a `Button` declaring `frame` names them itself.

### Wheel surfaces

A `Row` or `Column` declaring `write` is an interactive surface over its whole box (`SurfaceSpec`,
drawn by `widgets/wheel.rs::WheelSurface`). A wheel detent emits one signed
`ControlAction::StepScalar` and a double click emits `ControlAction::Activate`, both on the
container's own path. Such a container needs an `id`; `validate::check_module_node_ids` otherwise
rejects the document with `UnaddressedSurface`.

- A trackpad gesture steps by the sign of each pixel delta, no sooner than
  `WheelState::STEP_INTERVAL_MS` (200 ms) after the last step: `WheelScrolled` carries no scroll
  phase, so that interval is what bounds macOS momentum deltas after the fingers lift.
- A held press drags the same step at `WheelState::DRAG_STEPS_PER_PIXEL` (0.25) per pixel, measured
  from where the last step left off. The press consults the double click first, so the second press
  of a pair resets rather than drags.
- The surface counts steps and nothing else: what a step is worth and what the click returns to
  belong to the host owning the parameter.
- It claims the pointer over its whole box, reporting `ResizingVertically`, which in a `Stack`
  levitates the cursor away from everything it covers.

## Markup Composition

Cross-axis alignment is the container's: a `Row` centres its children, a `Column` leads them, and
every document in this crate, in `examples/gallery/assets` and in `crates/kithara-app/assets` takes
those defaults. A `Row` distributes no main-axis alignment, so a fixed-size cell centres its content
between two `Fill` spacers carrying no id; a leading indent spacer composes with its row's own
`gap`, and a trailing spacer belongs to the container whose bottom padding it reproduces. Checked-in
spec tables pin each node's own declared numbers and never a child's resolved position. A cell that
may leave a strip draws its own leading hairline rather than taking a seam from its neighbour, so a
cell the room or the host withdraws takes neither space nor seam with it.

## Parameter Substitution

A layout declares no parameters, so `expand::substitute` owns what `$` means at both levels: `$name`
in a layout-level `hidden` scope or in `Module.with` is a document error, `$$name` escapes a literal
`$name`, and inside a module the same function resolves `$name` against the instance's arguments. An
argument reaches a `String` field through `substitute`, an endpoint id through `substitute_binding`
(the substituted id being what the visitor hands `validate::check_controls`), and a typed field
through `param::Param<T>`. Serde tries the variant before the `"$name"` string, so a misspelt
variant parses as a reference - `resolve_param` spends the missing `$` on `BadVariant` and an
argument naming no variant on `BadParamVariant`, both carrying the node path.

## Module Chrome And Collapse Ownership

`ModuleDoc` owns optional shell labels, static assign labels and footer binding plus a typed
`ChromeStyle`: `Frame` (the serde default) renders the plain module frame, `Plain` only module
content, `Full` adds skin-owned header, separators and footer with assign labels as header chips.
Each layout module instance owns which outer frame sides render (`FrameSides`, all four true by
default) and whether decorative corner ticks draw, letting adjacent modules yield shared edges to
the layout grid. Collapse state is host-owned: a Full module reads `Bool` from
`ui.module.<module-doc-id>.collapsed` (absent means expanded) and header activation emits
`UiEvent::ToggleModule(<module-doc-id>)`, which the renderer neither retains nor mutates; Frame and
Plain modules ignore that endpoint.

A layout declaring `dragged` names what the pointer is carrying: while that binding reads as
non-empty text the renderer draws it at the pointer, over everything the layout lays out. The ghost
paints only - no event captured, no cursor claimed - and asks for a redraw as the pointer moves, so
following the pointer costs the host no messages. `SkinDoc.drag` owns its box and type; box
position relative to the pointer and label elision belong to the widget. A module declaring `drop`
emits
`ControlAction::Drag(DragPhase::Over(bool))` on `<instance>/drop` as the pointer crosses its bounds
and outlines itself while its `read` binding is true; `write` names the command the host runs on a
drop. The renderer never learns what is being dragged - the drag source reports its own start and
release on its own path, and the host decides what a drop means.

## Optional Block Ownership

`Optional` wraps exactly one node and marks it a block the host may hide. It is a wrapper rather
than a field on `Row`, `Column`, `Include` or `Module`: those own layout, and optionality is a
separate responsibility. Visibility is host-owned - the document names the endpoint through `hidden`
and kithara-ui invents none, the binding reads as `Bool`, and an absent read means visible.
`validate::value_kinds` owns that kind for a module-level wrapper, `validate::check_layout_block` at
the layout level; the binding takes `$`-substitution and scoped-key resolution like every other.

A block is hidden by its parent skipping it while iterating children, so it may only sit where some
parent iterates: a `Split` child, or a `Row`, `Column` or `Slot` child. `validate` rejects it
anywhere else - layout root, module root, directly under another `Optional`, and under a `Popover`
anchor/content or a `Pressable` - and the rule is total, so renderer and sizer need no case for a
hidden node handed to them directly. A block that must withdraw a click target wraps the
`Pressable`, not the glyph inside it; a fixed cell holding the slot open stays outside the block.

A block address names read state, so neither `.` nor `@` may appear in it, and
`validate::check_block_path` applies that to the whole expanded chain of enclosing ids. The id
shares the one namespace its siblings use: a module block id collides with a control id, a layout
block id with a module instance. The compiled block keeps the expanded path, addressable like a
control.

Sizing with blocks: a visible wrapper is fully transparent and delegates to its child,
`content_size` and `effective_size` included, so it never reaches the undeclared-size mapping in
`content_size`. A container decides emptiness over the children it actually lays out - a `Slot`
whose only child is hidden has the `SizeSpec::FILL` of an empty one, a gap is charged between
visible children only, and an all-hidden `Split` folds to `Dim::Fixed(0.0)`. `size::BlockNode` is
the single owner of "which block does this node declare", implemented once for `ExpandedNode` and
once for `CompiledNode`, so `size::visible` filters both stages through one predicate built from
`read::read_flag` - which makes a `Command` endpoint resolve to nothing.

A subtree's intrinsic size is a function of the node and the host snapshot, constant when neither an
optional block nor an adaptive node sits below it. `CompiledNode` records that in `blocks`, which
`size::has_blocks` sets, and `render/tree/size.rs::node_size` returns the precomputed
`compile::compiled_node_size` for such subtrees - memoization, not a fallback path.
`size::Snapshot` is the single description of what the host answers about a tree, one method per
question the sizer asks: which blocks are hidden, and what each adaptive measure reads.
`size::DEFAULTS` is the state before any answer, `CompiledUi.size` the size function evaluated in
it, and `render/tree/read.rs::Answers` the one implementation that reads a host, so snapshot-aware
sizing stays toolkit-independent.

## Adaptive Branch Ownership

`Adaptive` declares one place in several forms and draws exactly one of them: `base`, plus `steps`
ordered by the `from` threshold each takes effect at. The selected branch is the last step whose
`from` the measured value reaches, `base` below all of them - a step function over the whole real
line, not a chain of attempts. `expand::adaptive_branch` is that function, pure in the thresholds
and the value alone. The measured number belongs to the host endpoint, not to the node, and two
axes are two nested adaptive nodes.

- `measure` reads `ValueKind::Scalar` (`validate::value_kinds`), and the read is total in one place:
  `render/tree/read.rs::read_measure` answers `None` for a missing read, a value of another kind and
  a non-finite cast. `None` is `base` by contract, the rule that also makes an unread `Optional`
  visible and an unread `Popover` closed.
- `validate::check_adaptive_steps` requires at least one step, each starting at a finite value above
  the one below it; that order is what makes the selection total without a tie-break. Thresholds are
  document literals in whatever unit the endpoint measures.
- A node measuring `Width` or `Height` reads the box the toolkit gives it and must declare that
  axis: `validate::check_measured_box` rejects an absent `size` and a `Dim::Shrink` on that axis
  with `UiDocError::UnmeasuredAxis`, and a read measure declaring a box at all with
  `MeasuredBoxWithoutAxis`. Such a node answers its declared box whatever branch it draws, which
  keeps the pick from moving the siblings whose room decided it and lets `size::has_blocks` call the
  subtree constant. `widgets/adaptive/measured.rs` builds every branch once and lays out the one
  that fits, so a pick costs no rebuild and each branch keeps its state.

The node contributes no segment to the addresses below it: `expand::machine::expand_adaptive` spends
its own `id` on the visitor and its binding, then walks every branch with the enclosing context.
Repeating an `id` across branches is the point - one address, one host handler - and
`validate::walk_branches` unions each branch's claims from a clone of its parent's ids, so ids
collide inside a branch and never across them. A node reading its measure declares no `size` and
measures as the branch its snapshot selects, in `size::compute_size` and `geometry::effective_size`
alike, so a wrapper above it reports the drawn branch's size the way it reports an `Optional`
child's. Every branch expands at compile time, so the arena and the node budget carry them all.

## Threshold Reveal Ownership

A `Row` or `Column` declaring `measure` reads the box it is given on that axis, and each child
wrapped in `Reveal { from, until }` stands while that axis is inside the band: `from` is reached at
its own value and `until` is not, so two bands sharing an edge hold one place with exactly one cell
in it at any width. `size::stands` is that rule, called by both the compile-time filter and the
widget. Several children stand at once, which is where this differs from `Adaptive`; a child
carrying no wrapper always stands.

- The container obeys the same declared-box rule a self-measured `Adaptive` does, through the same
  `check_measured_box` and `UnmeasuredAxis`, so it answers its declared box however many children it
  shows and `size::has_blocks` calls it constant. A measuring `Column` is therefore handed both axes
  of its box, where a plain one leaves its height to its content.
- That box is the answer while every ancestor leaves the axis free. An ancestor asking for an
  intrinsic size sets `Limits::compression` on that axis, `iced`'s `Limits::resolve` then takes the
  intrinsic size over `Fill`, and the container answers the room its shown cells take while the
  threshold still reads the maximum it was handed. Validation lets such an ancestor through, so a
  document that puts one there gets a box that moves with its cells inside a subtree `has_blocks`
  calls constant.

Only that container answers a threshold, so a `Reveal` stands only among its direct children;
anywhere else is `UiDocError::UnmeasuredReveal`. `validate::Sibling::Measured` is what a measuring
container passes its children and `Sibling::Only` what a `Reveal` passes its own, so an `Optional`
directly below one is rejected too. A threshold is finite and not negative
(`UiDocError::RevealThreshold`), a ceiling finite and above the threshold it closes
(`UiDocError::RevealBand`), and `until: None` names no ceiling; bands carry no order among
themselves, unlike `Adaptive` steps. A ceiling can only take a cell out of a room, never put one in,
so the rooms `size::rooms` asks about stay the same set: the container's own minimum and every
`from` above it. The wrapper carries no `id` and contributes no segment to the addresses below it,
so a control inside one is addressed exactly as a control beside it.
`render/tree/node.rs::revealed` filters the host-hidden children through `size::visible` first, so
an `Optional` may stand among `Reveal`s as a plain always-revealed cell. The threshold is read
against the box the container declares, padding included: `widgets/adaptive/revealed.rs` charges
`pad`/`pad_x`/`pad_y` inside itself.

A layout `Split` asks the same question of whole modules: declaring `measure` reads the box it is
given, and each `SplitChild` names the band it stands in beside the weight it takes, so the band
sits on the child rather than in a wrapper. The arithmetic is the one value `size::Cells` - a row, a
column and a split hand it their cells and ask `need`, `settled` and `rooms`, so there is one climb
and one `size::stands` behind every threshold in the crate. `room::check_layout_cells` holds each
band against the tree behind it, the check a nested `Adaptive` could not give: a step answers only
its base's minimum, while a cell answers its own.

`Revealed` lays the shown children out through `iced`'s own `layout::flex::resolve`, so `gap`,
padding, alignment and `Fill` distribution are the toolkit's rules. That resolver takes one
contiguous slice, so the pass rotates the shown children to the front of both the element list and
the state list and rotates them back before it returns: widget state is bound to position, and
`Tree::diff_children` must find the order the element tree was built in. `draw`, `update`,
`mouse_interaction` and `overlay` address the shown children alone, so a hidden cell holding an open
`Popover` floats nothing. Neither widget under `widgets/adaptive` forwards `Widget::operate`, so
focus and programmatic scroll-to stop at the container.

## Minimum Room Ownership

A tree answers two questions. `size::compute_size` answers what box a node shows its parent, where a
declared `size` replaces everything below it - that constancy is what lets `size::has_blocks` call a
self-measured subtree memoisable. `size::min_size` answers how much room the node actually needs,
and there a declared box is a floor rather than the answer: the per-axis maximum of the declared
minimum and what the children compose to, so a box cannot swallow the room its cells take.
`min_size` takes no `Snapshot` - it is a property of the document and the skin alone.

A `Control` needs what `compute_size` answers for it, a declared box included; `Popover` needs its
anchor, `Pressable` its child, and `Scroll` its declared box when it has one. An `Optional` child
counts as standing - the host owns that switch, and a bar that overflows the moment the host shows a
block again is a bar that never fitted - while a `Reveal` child is the opposite, the room owning it,
so a cell waiting for a threshold contributes nothing on either axis until the room reaches it. An
`Adaptive` needs its base branch, the form it draws in the smallest window; what its steps need is
the separate question `room::check_steps` answers. `compile::compiled_min` carries the question up
the layout tree - `Split` settles its cells, `Optional` takes its child, `Adaptive` takes its base,
a `Module` takes its expanded root plus chrome - and `CompiledUi::min` is the answer at the root,
the number a host holds its window to; `compiled_min` is public so a host can ask the same of one
branch behind a threshold.

Which cells a measuring container stands at its own minimum depends on that minimum, so
`size::Cells::settled` resolves the circle by climbing from what the cells waiting for nothing take.
A round may only raise the answer - the climb is monotone because it is held so - and its fixed
point is a room whose own standing cells fit in it, bounded by a round per threshold and two more. A
container reading no room stands every cell, which is what `render/tree/node.rs::render_node` draws
for a `Reveal` outside a measuring container.

Three rules follow, all checked at compile time against the skin, because each is a question a
document can only answer once its controls have sizes.

- A step draws in the room its threshold promises: `room::check_steps` requires
  `step.from >= min_size(step)` on the measured axis, for every `Adaptive` reading `Width` or
  `Height`. A `MeasureSpec::Read` step is left alone - its threshold counts whatever its endpoint
  counts, and pixels are not that unit. Failure is `UiDocError::AdaptiveStepRoom`.
- A threshold names room the container has: `room::check_cells` requires that the cells standing in
  a room fit in it, at the container's own minimum and at every threshold above it - the standing
  set stands still between those points, so they cover every width rather than sample it. A
  threshold below the minimum is no error. Failure is `UiDocError::RevealRoom`.
- A node holds what its box can take: `room::check_box` requires, on both axes, that a declared box
  naming a ceiling - `Dim::Fixed`, or `Dim::Range` with a closed top - covers what the node
  composes, and `compile::Compiler::build` asks the same of a layout's own boxes. `Dim::Fill`,
  `Dim::Shrink` and an open range name no ceiling and are left alone, as are a `Scroll` and a
  `Control`, whose box overrides the skin on purpose. Failure is `UiDocError::DeclaredRoom`.

## Icon Identity

A document names an `IconName`; `render/tree/icon.rs::render_icon` joins it to `render::Icon`
(`render_tree_icon` joins the host-facing `TreeIcon` the same way), and
`render/icons.rs::source` joins `Icon` to a lucide glyph or an embedded SVG. The legs are guarded
differently: coverage by an exhaustive match with no wildcard, so a new `IconName` does not build
until given an arm; which glyph an arm names by a runtime table in that file's test module, compared
by codepoint because `lucide_icons::Icon` has no equality. Role-named variants are the drift surface
- `Faders`, `Collection`, `Charts` and `Waveform` each have a lucide-named neighbour that looks like
a plausible home - so the guard asserts both directions. New variants take the lucide glyph name in
UpperCamel.

## Container Press Ownership

`Pressable` makes any subtree a click target: `ControlAction::Activate` on a left press,
`ControlAction::SecondaryActivate` on a right press, both on its own path inside `UiEvent::Control`.
It is a wrapper rather than a `press` field on `Row` and `Column` because `validate::value_kinds`
answers per variant: a `press` field would make `Row` answer two write kinds depending on which
field is populated. As a variant, `Pressable` answers `(None, Some(Trigger))` while `Row`/`Column`
`write` keeps its wheel meaning (`Some(Scalar)`).

The innermost target wins with no hit-testing: `mouse_area` forwards the event to its content first
and returns as soon as the shell reports it captured. That capture is pass-global -
`Shell::capture_event` sets one sticky flag every sibling in the pass shares - so a `Pressable` is
suppressed by anything that captured earlier in the same traversal, not only by its own descendants:
harmless beside presses gated on the cursor being over their bounds, not harmless beside a widget
that captures away from the cursor, such as a `WheelSurface` or a track-list row mid-drag. A
`Button` inside a `Pressable` resolves by phase: `button` captures the press and publishes on the
release, `mouse_area` publishes and captures on the press, so the press is the button's and the
`Pressable` has no release to fire.

## Popover Ownership

`Popover` floats one subtree over the layout while a host-owned Bool reads true; an absent read
means closed. Only `anchor` is laid out in flow and it alone owns the node's intrinsic size;
`content` is measured inside the overlay and contributes nothing to the parent. The node carries no
`size` field - the content column declares its own width and the widget draws the chrome outward of
it. An optional block below `content` is charged, validated, interned and given a `BlockSpec` like
any other, yet `size::has_blocks` stops at the anchor, so the enclosing module records
`blocks: false`: this is the one place where "a block sits below" and "the size can change with the
snapshot" diverge.

A popover must not open inside another popover's content, enforced during expansion
(`Expander::in_popover`) rather than in the per-document node walk: expansion is the only stage that
sees across `Include` boundaries, and a `Popover` is legal at a module root, so a document-local
rule would let `Popover(content: Include(m))` through whenever `m` declares one.

The document names which geometry the surface opens from and which of its edges lines up with it;
the widget owns how. `at: PopoverAt` chooses anchor rectangle or pointer, defaulting to `Anchor`;
`align: PopoverAlign` picks the edge, defaulting to `Start`. A menu on a full-width row needs
`Pointer` (an anchor rectangle spanning the list cannot say where the user clicked); a burger under
a fixed cell needs `Anchor`. Everything else stays in `widgets/anchored.rs`, which owns the whole
pop chrome `SkinDoc.pop` declares - background, frame, gold cap, shadow - while markup declares only
content, frame and cap drawing outward of the content column.

`place` opens below the chosen geometry, overhangs the aligned edge by `FRAME_OVERHANG` (1 px) so
the content column lands flush, flips above when the room below runs out, and clamps into the
viewport. A `Pointer` popover opens at the press that opened it: `Anchored` records
`Cursor::position` on every `ButtonPressed` and `latch` consumes it on the false-to-true edge of the
open flag, so no banked press places at the anchor. The flip frame cannot read the live cursor -
`iced_runtime` hands the base tree `Cursor::Unavailable` while the overlay claims any interaction -
and the surface claims the cursor over itself and nowhere else, since claiming wider would cost the
whole base tree its cursor for that pass, the signal deciding whether the pointer belongs to the
overlay.

`Anchored` holds exactly two children: a closed popover's content is `iced::widget::Space` rather
than a dropped child, so the splits in `Widget::overlay`, `draw` and `update` are total patterns and
not fallback branches, and `Tree::diff_children` rebuilds the content state on each open. The
overlay's layout node is the whole surface with the content column as its single child, so
`layout.bounds()` means "the popover" everywhere and a press on the frame belongs to the menu.
`Widget::overlay` always wraps in `overlay::Group`, including a group of one: without it the layer
is bounded by the bare `Anchored` element and the shadow blurred outward falls outside it.
`Anchored` declares no overlay index and is the crate's only overlay-producing widget.

Dismissal fires only on a mouse press with the cursor outside the surface, or on Escape - never on a
release, a move or a scroll, since the opening press is captured while the popover is still closed.
The widget publishes `ControlAction::Activate` on the `Popover`'s own path, never the anchor's, and
captures the event, so the host's popover handler is set-false and never a toggle, the anchor's
press stays the only toggle, and outside press and Escape are idempotent.

## Shipped Documents

`builtin::resolver()` answers for every document below, and each exists in exactly one copy.

- `assets/modules/app-menu.kmodule.ron` and the row templates in `assets/modules/app-menu/` are the
  burger cell and the popover behind it. A host of `MICRO_PRESET` owes the window-manager endpoints
  the menu names: the window list, the per-window module flags and the saved layouts. Each row kind
  is one template taken as often as the menu needs through `Include`, and each instance's control
  paths are `app-menu/<include id>/<node id>`, so a template's own ids stay plain.
- `assets/modules/deck-micro.kmodule.ron` is the `MICRO_PRESET` window and
  `assets/modules/deck-micro/bar.kmodule.ron` its bar: a `Column` measuring `Height` over a `Row`
  measuring `Width`, so the window takes the deck and the library as it grows taller and the bar
  takes its cells as it widens. The bar stands the menu at every width.
- `assets/modules/master-clock.kmodule.ron` and `assets/modules/pivot-portals.kmodule.ron` are
  component documents over host-owned timing state: a production host supplies its measured clock
  sources, transport state and portal policy, and the documents retain no tempo, source, range, Link
  or MIDI state. The clock's popover surface is a document of its own, so the standalone module and
  the micro bar open one panel. The deck key-lock, overview row and portals stay outside the builtin
  preset surface: the Gallery embeds them explicitly and supplies mock endpoints.
- `assets/modules/deck/quality.kmodule.ron` is the deck's stream-quality cell and its menu, taken
  per deck through `parameters: ["deck"]`. It is markup over `Popover`, `Pressable` and the row
  templates in `assets/modules/deck/quality/`, so the crate gains no control for it; its surface is
  the crate's own `Pop` chrome, which is where it departs from the design's `HlsCell`. The ladder is
  host-owned throughout: the cell is an `Optional` block the host hides on a plain file, each rung
  is an `Optional` slot, automatic selection is a sibling template pinned to `variant: "auto"`, and
  the popover's own path carries the set-false the widget publishes on dismissal.

`PortalMap` and `Range` are the dedicated renderer primitives in that group. `PortalMap` copies one
`PortalMapView` snapshot for the iced canvas lifetime and draws the declared arcs with
`SkinDoc.portal_map` geometry. `Range` reads normalized bounds and emits a scalar at
`<control-path>/min` or `<control-path>/max`; the host owns snapping and the minimum gap. Selection,
portal enumeration and timing decisions remain host-owned.

## Window Chrome Ownership

A layout setting `resize_edges` is framed by `render::tree::render` with the eight drag zones a
system border would have given it. They lie over the content, not beside it, so declaring them costs
the layout no space; `SkinDoc.window.resize_edge` owns their thickness, and the host maps each
`WindowEdge` to its toolkit's resize direction.

`WindowDrag`, `TitleBar` and `WindowControls` paint no surface of their own and take their size from
the row that holds them, so the same controls sit in a gallery header and in an app bar; the
document declares the background. `WindowDrag` is the bare drag surface a bar without a title needs,
`TitleBar` the same surface with a label, and both emit on press rather than on release. All three
are portable, binding-free controls emitting typed `UiEvent::Window(WindowCommand)` values; the host
owning the native window ID executes drag, minimize, maximize and close. `kithara-ui` owns their
declarative schema and skin-driven presentation, never native window state.

## Track List Column Ownership

`TrackList` owns an ordered typed `Vec<TrackColumn>` and requires `Title` during compilation. The
renderer owns table geometry and cell presentation but not column state: with a `columns_state`
binding present the host exposes Bool visibility reads at `<binding-id>.<column endpoint_name>` and
Scalar widths at `<binding-id>.width.<column endpoint_name>`, with `SetScalar` controls emitted at
`<track-list-path>/width/<column endpoint_name>`; a missing derived endpoint means visible, a
missing width read takes the skin default. The renderer retains only canvas drag state, clamps
resizable fixed columns to the skin minimum, and keeps the required Title column flexible.

The `Deck` column marks assignment and does not offer it: it shows the letters the host put on the
row, and nothing when there are none. A row is a drag source instead - pulling past
`behavior::ItemDrag::DRAG_THRESHOLD` (4 px) emits `ControlAction::Drag(DragPhase::Start(index))` on
the control path, and the release emits `DragPhase::Drop`. The gesture captures no event, so the row
keeps its own click and whatever the drag is released over sees the same release.

## Browser Tree Ownership

`Tree` reads a borrowed flat row slice whose depth, branch state, selection and presentation flags
are host-owned. The renderer never mutates or filters that state; activating any visible row emits
`ControlAction::SelectIndex` on the control path, and the host decides whether that index toggles a
branch or selects a leaf. `TreeSkin` owns search, row, indentation, panel and context-bar metrics.
`ContextBar` keeps breadcrumb text read-only; optional scope items use a separate Scalar read
binding and emit `SelectIndex` on the control path. `validate::check_context_scope` requires
`scope_items`, the scope read and the write to appear together or not at all.

## Application Consumer

The `kithara-app` GUI is the production consumer: it embeds its own layout and module documents,
implements `EndpointRegistry` (`gui/ui/endpoints.rs`) and `Reads` over an address tree
(`gui/reads/`), and maps `UiEvent` to app messages (`gui/ui/events.rs`).
