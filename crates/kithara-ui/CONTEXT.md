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
availability rather than skin design. `Skin` mirrors the document sections a renderer reads
often — `skin.menu`, `skin.pop` — as fields of its own.

The palette is the single colour vocabulary. A skin section names a `ColorRole` and never a hex,
and only `PaletteDoc::validate` parses one. Alpha stays a skin field beside the role it applies
to, in the shape `track_alpha`, `played_alpha` and `ShadowSkin.alpha` already carry; a hex with
an alpha byte baked in is what `accent_soft` is, and it has no design source and no consumer.

A `ColorRole` is an alias for a value, and several role names do not carry the design token they
sound like. Select a role by the value it holds:

| role | hex | design token |
| --- | --- | --- |
| `BgFooter` | `#1b1b32` | `panel2` |
| `BgSelect` | `#26264a` | `select` |
| `BgPanel2` | `#26264a` | none — duplicate of `BgSelect` |
| `LineInner` | `#2a2a4c` | `lineDim` |
| `LineSoft` | `#2a2a4c` | none — duplicate of `LineInner` |
| `LineDim` | `#242442` | none |
| `AccentSoft` | `#bb94422e` | none |

Two pins guard the palette, one property each.
`doc/skin.rs::palette_holds_exactly_the_declared_roles` compares the parsed document against a
whole-struct `PaletteDoc` literal, so a field added to or removed from the struct does not compile
until that literal names it: this pin owns completeness and every hex. `tests/skin.rs::TOKENS` is
the checked-in `(design token, hex, ColorRole)` table, which makes a design change reviewable by
token name rather than by hex. It covers the fields that carry a token — `bg_panel_2`, `line_dim`,
`line_soft` and `accent_soft` have none and appear in no row — and its length assert pins the
written list against the table rather than against the struct, so a new field takes a token row by
review and never by the assert.

`SkinDoc.menu` owns the four menu icon sizes. Menu typography resolves through `SkinDoc.text`,
which holds one entry per type spec — family, weight, size, letter-spacing and the colour that
spec carries by default — so a tone that differs from the default is named on the node and
mints no second entry. A `Dim` is a literal with no role indirection, so a `.kmodule.ron` cannot
name a skin metric; menu row heights live in the markup, which is where the renderer reads them,
and a second home in the skin would be a number nothing consults.

A frame a document declares is a `Canvas` stacked over the container's background, which is what
lets one node draw a hairline on chosen sides in a colour of its own; the background underneath it
is a plain container fill with neither border nor shadow. Neither carries a shadow, so a document
node cannot cast one. The pop-over is the one surface that needs both, and `Anchored` draws its
background, frame and shadow as a single iced `Quad` — the border inside the bounds, the shadow
around the same rectangle. `SkinDoc.pop.frame` declares radius `0.0`, so surface, border and
shadow share one square outline.

## Wave View Ownership

Hero-wave zoom and playback position are host-owned scalar state. An optional `Wave.zoom` binding
reads the visible track fraction; wheel interaction emits `SetScalar` at `<wave-path>/zoom`, while
horizontal drag emits `SetScalar` at the wave path for the host-owned playback position. The
renderer keeps neither value and derives the centered zoom window from each read snapshot.

`zoom_math` owns the zoom scale for every widget that draws a wave: the window the wave opens
with, the bounds it clamps to, and what each gesture is worth. A wheel detent and a zoom button
step by different factors because they are different gestures — the detent is the finer one. A
host that drives zoom from a button rather than from the wave takes `render::zoom_in` and
`render::zoom_out` and gets the same bounds the wave draws within.

The hero wave dims the track left of the playhead with `SkinDoc.wave.played_alpha` mapped through
its zoom window; the bars style dims the full track with `SkinDoc.wave.overview_played_alpha`. The
micro style carries no playhead dimming.

## Active Tone Ownership

`Text`, `Glyph` and `Row` each carry an optional `active` binding — one shape across three
carriers. It is a Bool the host owns, read through the one path every binding read takes, and an
absent read means inactive. `Text` renders the content its `read` binding or `label` supplies;
`active` selects only a tone, never content.

`render/tree/geometry.rs::active_tone` is the single selection rule: the active role when the node
is active and declares one, otherwise the base role. A node that binds `active` while naming no
active role therefore keeps its base tone.

A `Text` and a `Glyph` each name their own `ColorRole` pair through `color` and `active_color`,
selecting among palette roles the way `Row.background` does, while the skin keeps every metric and
the palette stays the single colour vocabulary. A node that names no colour takes the one its skin
entry carries. `Glyph` sizing stays skin-owned through `GlyphStyle`, and `active_icon` switches the
glyph itself, which is how one caret is one node with one path and one style declaration.

`widgets/text.rs::text_role` is the one `TextStyle` to `TextRoleSkin` join, and it feeds
`active_tone` the node's pair with the skin entry's active colour behind it —
`text.deck_letter_active`, which marks the focused deck. The match carries no wildcard arm, so a
new style must be given a skin entry.

`Row` alone carries `active`, `active_background`, `frame_color` and `active_frame_color`;
`Column` carries none of them, because nothing declares them on a column and a pop-over's frame
belongs to the widget. `frame_tone` resolves the frame pair, giving a node that names no colour
the skin divider, and `widgets/chrome.rs::frame_overlay` takes colour and width as
arguments and reads no skin section — which is what lets one surface carry two hairline colours.

An `active` binding needs no `id`, and the shipped App Menu relies on that. `validate` requires an
id only for a container that declares `write`, and `expand::machine::container_bindings` addresses
an id-less container as its own module: its `ControlSite` path is the enclosing prefix — the
module instance, or the `Include` chain above it — which every id-less sibling shares. That
sharing is sound while the visitor body stays validation only — `validate::check_controls` spends
the path on error context and keys nothing by it, and a container without `write` yields no
`SurfaceSpec`, so no shared path reaches the compiled tree. Every binding resolves by its own
scoped key and never by the node path; a visitor that kept state per `ControlSite.path` would need
the id rule widened first.

`text_role` lives in `widgets/text.rs`, `glyph_tone` in `render/tree/atom.rs`, and `frame_tone` in
`render/tree/geometry.rs` beside `active_tone`, the rule it routes through: `render/tree/mod.rs`
keeps `geometry` private, so a join placed under `widgets` cannot name that rule and would have to
carry a second copy of it. Both `frame_tone` call sites are in `render/tree/node.rs`.

Each join is pinned by a `#[cfg(test)]` module beside it, asserting both polarities against the
skin field the style selects rather than against the value the skin holds. Those are the assertions
that catch a swapped match arm, which no table of design values can see.

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
bare and callers compose no separate text node beside it.

A waveform column is one bar wide for all three bands: low, mid and high are drawn from the
vertical centre over each other and nest by level, never by width, so a single `bar_width` and
`bar_gap` set the column pitch for both the deck wave and the overview row.

Controls take their size from the wrapper, not from themselves: a widget fills what
`size::control_size` or the document gives it. A widget that pins its own height would ignore the
document and break the row it sits in. The declared size is the whole control box: the knob's
caption row sits inside it whether or not a label fills it, and the dial is what remains, so a
28 px dial asks for a 39 px box and a square declaration renders a squashed dial. A knob that
declares no size takes `skin.knob.size`, which is the one place those two numbers are kept
together.

A menu glyph is the one control whose declared size names one axis only. It draws as a text
glyph, so its box is the icon size wide and a line box tall — `1.3` times the size, the iced
default — and a square declaration is shorter than what it draws. The paragraph then sits at the
top of the box it overflows, which reads as an icon sunk below the label beside it. `icon_cell`
therefore fixes the width and leaves the height to the row, which is what centres the glyph
against its siblings.

A transport cell carries its hairline on the sides `SkinDoc.button.transport_sides` names; a
`Button` that declares `frame` names them itself. One seam stands between neighbouring cells and
none at the strip's ends, so the cells before its flexible gap keep the skin's right seam while
the cells after it take a left one, the side the container cells beside them already declare.
Only the transport styles read that declaration; every other button style draws the border of its
own style.

A container that declares no `size` renders `Fill` on both axes — `render::tree::content_size`
maps the undeclared case there. A stack of unsized rows therefore splits its parent between them
instead of hugging its content, and a row that should hug says `h: Shrink`.

`Dim::Shrink` is the one rule the document layer cannot compose: the toolkit measures the content,
so `Bounds` treats it as an open axis and `Dim::from(Bounds)` never produces it. A shrunk node
therefore has to carry `Shrink` down to its own children — `render::tree::content_size` passes it
to the container, its frame overlay and its fill, because the first `Fill` inside a shrunk box
claims the whole row. Text measures its glyphs and takes alignment from that wrapper; a readout
that draws its own framed cell keeps filling the box the document gave it.

A `Row` or `Column` that declares `write` is an interactive surface over its whole box: a wheel
detent there emits one signed `ControlAction::StepScalar`, and a double click emits
`ControlAction::Activate`, both on the container's own path. A trackpad gesture steps by the sign
of each pixel delta, no sooner than `WHEEL_STEP_INTERVAL_MS` after the last step:
`iced::mouse::Event::WheelScrolled` carries no scroll phase, so the momentum deltas macOS keeps
sending after the fingers lift are indistinguishable from the gesture, and that interval is what
bounds how far they carry the value. A held press drags the same step at `DRAG_STEPS_PER_PIXEL`
per pixel, upward for a step up, measured from where the last step left off so the surface never
needs the value; the press consults the double click first, so the second press of a pair resets
rather than drags. The surface counts steps and nothing else. It reads no value and holds no range, so
what a step is worth and what the click returns to belong to the host that owns the parameter. It
claims the pointer over its whole box: it reports the `ResizingVertically` the knobs report, which
in a `Stack` levitates the cursor away from everything the surface covers. Such a container needs
an `id` to be addressed by; `validate::check_module_node_ids` rejects the document otherwise. A
container that stays silent about `write` carries no write path.

`validate::value_kinds` is the single owner of control read/write endpoint kinds. Intrinsic sizes
are selected exhaustively from `ControlSpec` and the supplied `SkinDoc` by
`size::control_size`; this remains available in non-render and wasm builds. Renderers match
`ControlSpec` directly and do not resolve a runtime control catalog.

## Markup Composition

Cross-axis alignment is the container's, and the two containers differ on purpose. A `Row`
centres its children, because controls of differing heights share a baseline. A `Column` leads
them, because its cross axis is text flow. Every document in the crate and in
`crates/kithara-app/assets` takes those defaults; a column that wants its children centred
composes it with spacers.

A `Row` distributes no main-axis alignment, so a fixed-size cell centres its content by
composition: the content sits between two spacers that are `Fill` on both axes. Those spacers
carry no id, so no spec table names them.

A leading indent spacer composes with its row's own `gap`, which is inserted after the spacer as
well as between every other pair, so the indent a design pins is the spacer plus the row's padding
plus that gap. A trailing spacer belongs to the container whose bottom padding it reproduces, as
that container's last child.

The checked-in spec tables pin each node's own declared numbers and never a child's resolved
position, so no row of theirs can assert a composed offset.

## Module Chrome And Collapse Ownership

`ModuleDoc` owns optional shell labels, static assign labels, and footer binding plus a typed
`ChromeStyle`. `Frame` is the serde default and renders the plain module frame; `Plain` renders
only module content; `Full` adds the skin-owned header, separators, and footer. Assign labels
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

## Optional Block Ownership

`Optional` wraps exactly one node in a module tree or a layout tree and marks it a block
the host may hide. It is a wrapper rather than a field on `Row`, `Column`, `Include` or
`Module`: those own layout, and optionality is a separate responsibility.

Visibility is host-owned. The document names the endpoint through `hidden` and kithara-ui
invents none; the binding is read as `Bool` exactly like `Text.active` and `ModuleDrop.read`,
and an absent read means visible. `validate::value_kinds` owns that kind for a module-level
wrapper and `validate::check_layout_block` applies the same kind at the layout level. The
binding takes `$`-substitution and scoped-key resolution like every other, so
`with: { "deck": "$deck" }` gives per-deck visibility without a crate-level scoping rule.

A block is hidden by the parent skipping it while iterating its children, so a block only
ever occupies a position some parent iterates: a `Split` child, or a `Row`, `Column` or
`Slot` child. `validate` rejects it anywhere else — at a layout root, at a module root, and
directly under another `Optional` — because in those positions nothing iterates past it and
the block could never be hidden at all. The rule is total, so the renderer and the sizer
need no case for a hidden node they were handed directly.

A block that must withdraw a click target along with what it draws wraps the `Pressable`, not the
glyph inside it. The fixed cell holding the slot open stays outside the block, a plain container
with its own id, so hiding the block moves no neighbour.

A block address names read state, so neither `.` nor `@` may appear in it. The address is
composed from every id enclosing the block, so `validate::check_block_path` applies that rule
to the whole chain — a `mixer.a` module instance may not enclose a block, however clean the
block's own id is. The id also shares the one namespace its siblings use: a module block id
collides with a control id, and a layout block id collides with a module instance. The
compiled block keeps the expanded path — `mixer/eq`, `deck-a/transport/eq` — so it is
addressable by the same form a control is.

A layout declares no parameters, so `expand::substitute` owns what `$` means at both levels:
`$name` in a layout-level `hidden` scope or in `Module.with` is a document error, and `$$name`
escapes a literal `$name`. Inside a module the same function resolves `$name` against the
arguments the instance was given.

An argument reaches three kinds of field. A `String` field takes it through `substitute`. An
endpoint id takes it through the same call in `substitute_binding`, so a template names the
endpoint it reads; the substituted id is what the visitor hands `validate::check_controls`, and
the registry still answers for it. A typed field takes it through `param::Param<T>`, which reads
either the variant itself or a `"$name"` string — serde tries the variant first, so a spelled-out
one never reads as a reference. The cost of that order is that a misspelt variant parses as a
reference instead of failing on the spot, so `resolve_param` spends the missing `$` on
`BadVariant` and an argument that names no variant on `BadParamVariant`, both carrying the node
path.

A visible wrapper is fully transparent and delegates to its child, `content_size` and
`effective_size` included, so it never reaches the undeclared-size mapping in `content_size`.
A container decides emptiness over the children it actually lays out: a
`Slot` whose only child is hidden has the `SizeSpec::FILL` of an empty one, and a gap is
charged between visible children only. An all-hidden `Split` folds to `Dim::Fixed(0.0)`.

`size::BlockNode` is the single owner of "which block does this node declare", implemented
once for `ExpandedNode` and once for `CompiledNode`, so `size::visible` filters both stages
through one predicate. The renderer builds that predicate from `read::read_flag`, the one
path every binding read takes, which is what makes a `Command` endpoint resolve to nothing.

A subtree's intrinsic size is a function of the node and the visibility snapshot, and
constant when no optional block sits below it. `CompiledNode` records that at compile time
in `blocks`, and `compile::node_size` returns the precomputed size for such subtrees rather
than walking them once a frame; this is memoization of a pure function, not a fallback path.
`CompiledUi.size` is that function evaluated with every block visible, which is the layout's
size whenever no document below it declares a block. `size::compute_size` takes the snapshot
as a predicate over `BlockSpec`, so visibility-aware sizing stays toolkit-independent and
available in non-render and wasm builds.

## Icon Identity

A document names an `IconName`; `render/tree/icon.rs::render_icon` joins that to `render::Icon`,
and `render/icons.rs::source` joins `Icon` to a lucide glyph or an embedded SVG. The two legs are
guarded differently because they fail differently. The first is a coverage question, and the
exhaustive match with no wildcard answers it at compile time: a new `IconName` does not build
until it is given an arm. The second is a value question no compiler can settle, since every arm
is well-typed whichever glyph it names, so it carries a runtime table in that file's test module,
compared by codepoint because `lucide_icons::Icon` implements no equality.

Role-named variants are the drift surface. `Faders`, `Collection`, `Charts` and `Waveform` are
named after what they are for rather than after their glyph, and each has a lucide-named
neighbour that looks like a plausible home for it. The guard therefore asserts both directions:
each menu glyph resolves to its namesake, and no role-named incumbent resolves to the neighbour
it could be mistaken for. New variants are named after the lucide glyph in UpperCamel, which is
what keeps the positive half a name identity.

## Container Press Ownership

`Pressable` makes any subtree a click target. It emits `ControlAction::Activate` on a left press
and `ControlAction::SecondaryActivate` on a right press, both on its own path and both inside the
existing `UiEvent::Control`, so a right-clicked node is addressed exactly as a left-clicked one is
and secondary click needs no separate routing.

It is a wrapper rather than a `press` field on `Row` and `Column` for the reason `Optional` is
one: those own layout, and interaction is a separate responsibility. The technical half is
`validate::value_kinds`, the single owner of read and write endpoint kinds, which answers per
variant — a `press` field would make `Row` answer two different write kinds depending on which
field is populated, the first write in the crate validated outside `value_kinds`. As a variant,
`Pressable` answers `(None, Some(Trigger))` and rides the existing write site unchanged, and
`Row`/`Column` `write` keeps its wheel meaning untouched.

The innermost target wins with no hit-testing: `mouse_area` forwards the event to its content
first and returns as soon as the shell reports it captured. That capture is pass-global —
`Shell::capture_event` sets one sticky flag every sibling in the pass shares, and nothing unsets it
within a pass — so a `Pressable` is suppressed by anything that captured earlier in the same
traversal, not only by its own descendants. Harmless beside presses gated on the cursor being
over their bounds; not harmless beside a widget that captures away from the cursor, such as a
`WheelSurface` or a track-list row mid-drag.

A `Button` inside a `Pressable` resolves by phase rather than by nesting. `button` captures the
press and publishes on the release; `mouse_area` publishes and captures on the press. So the press
is the button's and the `Pressable` stays silent, and the release is the button's and the
`Pressable` has no release to fire.

## Popover Ownership

`Popover` floats one subtree over the layout while a host-owned Bool reads true; an absent read
means closed, the default-state shape `Optional`'s "an absent read means visible" and collapse's
"an absent value means expanded" already carry. Only `anchor` is laid out in flow, and it alone
owns the node's intrinsic size — `size::compute_size` and `render/tree/geometry.rs`'s
`effective_size` both delegate to it. `content` is measured inside the overlay and contributes
nothing to the parent. The node carries no `size` field: the content column declares its own
width and the widget draws the chrome outward of it.

An optional block below a popover's `content` is charged, validated, interned and given a
`BlockSpec` like any other, yet `size::has_blocks` stops at the anchor, so the enclosing module
records `blocks: false` and the renderer answers its size from the memoized value instead of
walking the subtree. That is correct — content contributes no size in any snapshot — and it is
the one place where "a block sits below" and "the size can change with the snapshot" diverge.

A popover must not open inside another popover's content. The rule is enforced during expansion
rather than in the per-document node walk, because expansion is the only stage that sees across
`Include` boundaries: unlike `Optional`, a `Popover` is legal at a module root, so a
document-local rule would let `Popover(content: Include(m))` through whenever `m` declares one.
Enforcing it once, where the include graph is flattened, is what makes "no submenus" a schema fact
rather than a shape nobody happened to write.

The document names which geometry the surface opens from and which of its edges lines up with
that geometry; the widget owns how. `at: PopoverAt` chooses between the anchor rectangle and the
pointer, and defaults to `Anchor`, so a document that declares nothing opens under its anchor.
`align: PopoverAlign` picks the edge and defaults to `Start`: a menu wider than its trigger grows
rightward from it, and `End` grows leftward instead, which is what a cell at the right end of a
bar needs. The overhang follows the aligned edge, so the content column lands flush either way. Everything else stays in `widgets/anchored.rs`:
`place` puts the surface below whichever geometry it opens from, overhangs it a pixel to the left
so the content column starts flush, flips above when the room below runs out, and clamps both
axes into the viewport; a surface taller than the viewport starts at the top and overflows
downward. A menu on a full-width row needs `Pointer` — an anchor rectangle spanning the list
cannot say where the user clicked — and a burger under a fixed cell needs `Anchor`. That is the
whole of the choice, which is why the enum names geometry and not the menus that use it.

The point a `Pointer` popover opens at is the press that opened it, not the cursor of the frame
the flip lands on. `Anchored::update` records `Cursor::position` on every `ButtonPressed`, and
`latch` consumes that record on the false→true edge of the open flag, so a cursor moving over an
open surface never drags it and every open takes its own press. The flip frame cannot read the
live cursor: `iced_runtime` builds the overlay before it updates the base tree, and it hands the
base tree `Cursor::Unavailable` whenever the overlay claims any interaction — the open popover
covering the pointer is exactly that case. A latch that finds no banked press consumes `None`,
which places the surface at the anchor rather than failing to place it — the crate opens every
popover from a press, so that is the shape a keyboard-driven open would take.
`Widget::overlay` carries the latched point through the same translation as the anchor
rectangle, so both live in the overlay's space.

The surface claims the cursor over itself and nowhere else. Reporting the content's interaction
from outside the surface would cost the whole base tree its cursor for that pass, since that is
the signal `iced_runtime` reads to decide whether the pointer belongs to the overlay.

`SkinDoc.pop` is the sole owner of the pop chrome — background, frame, gold cap and shadow — and
`Anchored` paints all four. The markup declares only content; a document that redeclared any of
them would be a second owner of one value. Frame and cap draw outward of the content column, so a
298 px column yields a 300 px surface whose height exceeds the content by twice the frame plus
the cap.

`Anchored` holds exactly two children. `render/tree/node.rs` hands it `iced::widget::Space` for
the content of a closed popover rather than dropping the child, so the two-entry split in
`Widget::overlay` and the single layout child in the overlay's `draw` and `update` are total
patterns, not fallback branches.
`Tree::diff_children` rebuilds the content state on each open, which is what a menu wants. The
overlay's own layout node is the whole surface with the content column as its single child, so
`layout.bounds()` means "the popover" in every method and a press on the frame belongs to the menu.

`Widget::overlay` wraps whatever it produces in `overlay::Group`, including a group of one.
`overlay::Nested::draw` draws the top-level overlay element inside a layer bounded by that
element's own layout node. `overlay::Group::layout` returns a node the size of the whole viewport,
while a bare `Anchored` element is exactly the surface — so its shadow, offset below and blurred
outward, falls outside the layer. Skipping the `Group` when it holds one child deletes the shadow
and nothing else.
`Anchored` declares no overlay index, so its surface takes iced's default; it is the crate's only
overlay-producing widget.

Dismissal fires only on a mouse press with the cursor outside the surface, or on Escape — never on
a release, a move or a scroll. The press that opens the menu is captured while the popover is
still closed, and its release lands on the freshly built overlay with the cursor over the anchor,
so a release-based rule would close the menu the instant it opened. The widget publishes
`ControlAction::Activate` on the `Popover`'s own path, never the anchor's, and captures the event
so the anchor's press cannot also fire. Because the two paths differ, the host's handler for the
popover path is set-false and never a toggle, and the anchor's press stays the only toggle — which
is what makes an outside press and Escape idempotent.

`assets/modules/app-menu.kmodule.ron` and the row templates it includes from
`assets/modules/app-menu/` are shipped assets that `builtin::resolver()` deliberately does not
answer for. Their window-manager endpoints — the window list, per-window module flags, saved
layouts — are host state no crate owns, so the documents must not become canonical preset surface
the studio can resolve. Exactly one copy of each exists and every consumer reaches it with
`include_str!`.

The window row, the module-grid cell, the saved-layout row, the preference toggle and the toggle
that reports a hint are one template each, taken as many times as the menu needs through
`Include`. Each instance's control paths are `app-menu/<include id>/<node id>`, so the template's
own ids stay plain. The three rows carrying a literal shortcut and the settings row keep their own
geometry and stay written out.

## Stream Quality Ownership

`assets/modules/deck/quality.kmodule.ron` is the deck's stream-quality cell and its menu, taken
per deck through `parameters: ["deck"]`. It is markup over `Popover`, `Pressable` and the row
template in `assets/modules/deck/quality/`, so the crate gains no control for it; the cell is a
`Pop` surface like every other menu in the crate, which is the one place it departs from the
design's own `HlsCell`, where the list draws its own panel and shadow.

The transport bar is where it rides, before `TEMPO`, and it rides there as an `Optional` block:
a quality ladder belongs to the stream, and a track served as one file has none, so the host
answers `deck.stream.quality_hidden` and the cell leaves the row with its seam. The cell also
reads `deck.stream.buffer` for the buffer depth the design shows beside the value.

The ladder is host-owned and the document holds six slots for it. A slot beyond the ladder is an
`Optional` block the host hides through `deck.stream.variant_hidden`, and the row inside it names
`deck.stream.variant_label`, `deck.stream.variant_sub` and `deck.stream.variant_active` under the
same `deck` and `variant` scopes. Automatic selection is the `variant=auto` row: it takes the same
template, reads the same endpoints and writes the same `deck.stream.select_variant`, so the host
owns what automatic means and the markup lists one kind of thing.

The cell reads `deck.stream.quality` for what it shows and `deck.stream.quality_menu` for both its
open state and its own two-state fill; `deck.stream.toggle_quality_menu` is the only toggle, and
the popover's own path stays the set-false the widget publishes on dismissal.

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
