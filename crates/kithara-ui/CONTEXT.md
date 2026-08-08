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
colours and keeps it behind `Skin::document()` for layout sizing. Frequently read document sections,
including `menu` and `pop`, also remain resolved fields on `Skin`. `Knob` painting is toolkit-neutral,
but `atoms` remains render-gated because the knob consumes `render::Skin` and its interactive adapter
is an iced canvas program. The platform-specific monospace family stays code-owned in
`render/fonts.rs` - font availability, not skin design.

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

## Draw Ownership

`SkinDoc` owns document-level draw roles and metrics. `draw` owns the toolkit-neutral native
geometry primitives, retained commands, ordered `DrawList` value, builder, backend trait, and
replay. A widget owns command order and local geometry. A backend owns conversion from `Geom` into
its toolkit vocabulary, text submission, and encoding into its target. Geometry crosses the seam
as arcs, circles, lines, rectangles, and uniform-radius rounded rectangles rather than
pre-flattened paths, so every backend rasterises curves at its own device scale. A zero-radius
rounded rectangle is canonicalised by the builder to the existing rectangle command; its retained
list is therefore exactly the list callers produced before the rounded shape existed.

The retained list is a cloneable, comparable value. Cross-backend identity is therefore asserted
against the same list rather than promised by two call-through implementations. Pooling is
deliberately absent. A kithara-bufpool byte budget would not enforce a retained-command ceiling:
`Pool::track_byte_delta` runs only inside `Pool::acquire`, never when a caller grows a vector with
`push`. Any future cap must be a builder-side contract, not a pool property.

A viewport is retained as `DrawCmd::Clip { region, list }`: the region and the nested `DrawList`
travel as one scoped command. The nesting is required by iced, not a convenience chosen by the
builder. `Frame::with_clip` is iced's only public clipping route and receives a closure that draws
into a drafted frame; the lower-level draft and paste operations are private, so a neutral
push/pop pair could not be replayed against that backend. `Backend::clip` therefore receives the
region and borrowed nested list. The iced backend enters `Frame::with_clip` and recursively
replays into the drafted frame, while Vello pushes a clip layer, recursively replays into the same
scene, and pops the layer. The command tree is the clipping acceptance surface; tests assert the
nested commands rather than backend pixels.

`TickRail` paints through the builder, so the shared atom names no toolkit and `atoms::vu` draws
entirely through one seam. The button is the production consumer that asked for the rounded
rectangle: its fill and uniform-radius border stay one native shape in the retained list, iced
replays it with `Path::rounded_rectangle`, and Vello replays it as `kurbo::RoundedRect`. The
default fader also paints through the builder: two independently framed solid rail rectangles are
followed by the rectangular handle, preserving iced slider command order and geometry. The Volume
fader remains on its existing canvas painter; its engine-owned variant reuses that painter without
the interactive program. The crossfader remains on its existing iced path in this slice; the new
vocabulary does not by itself authorize that separate port.

### What a backend must be able to draw

Every backend states `const CAPS: Caps` once. `Needs::from(&DrawList)` reads what a list actually
asks for, recursing into clips, and a backend that lacks any of it is given **none of the list**:
`draw::replay` refuses and logs, and `backends::iced_canvas::replay_ordered` — the iced host's own
door, which walks the list itself rather than going through `replay` — repeats that check. Refusing
whole is the contract because painting the rest puts a picture on the screen that no list described:
a clip-less backend would spill a clip's contents, and a gradient-less one would flatten a ramp to a
colour appearing nowhere in the document. A backend that overstates `CAPS` therefore paints the
wrong thing silently; the conformance tests in `backends::conformance` rasterise through the real
renderers precisely because recorded commands cannot catch that.

Today only one capability differs between hosts: iced 0.14 has no radial gradient at any layer, so
`IcedBackend::CAPS` sets `radial_gradient: false` and a list holding `Paint::Radial` reaches the
Vello host only. `style()` answers `None` for such a paint rather than substituting a colour.

Stroke shape travels as `Pen { cap, join, width }`; `impl From<f32> for Pen` means a caller with a
bare width keeps working, the way `impl From<Rgba> for Paint` does for fills.

`backends::iced_canvas` owns iced translation, glyph-outline filling, and canvas calls.
`backends::vello` owns Vello translation and scene encoding; the draw list needs no GPU dependency.
Vello is held at 0.6 because masonry 0.4 hands a widget a `Scene` from that release, and `Scene`
from a different Vello version is an unrelated type. The direct Vello dependency keeps its `wgpu`
feature off because the backend only encodes commands. Enabling `masonry-host` still brings Vello's
wgpu 26 renderer through Masonry while iced brings wgpu 27. Cargo keeps the two majors distinct;
`deny.toml` reports multiple versions as a warning, so this coexistence is intentional.

`text::TextContext` is the canonical shaping owner. It owns Parley's font and layout contexts and
registers the embedded Inter, JetBrains Mono, Space Grotesk, and Lucide faces. A context is a
caller-owned value injected where shaping occurs; there is no process-global font context.

### Host layers

A `HostLayer<A>` is a host-owned output above the document: one window-space bound, one retained
local `DrawList`, and an ordered set of window-space `LayerHit<A>` regions that pair a rectangle
and cursor with a typed action. It is not a document node. It has no document path, never enters
`CompiledNode`, and is neither described nor reconciled by the document engine. The host rebuilds
it from current window state and replays its retained painting after the document. Window resize
edges and the drag ghost use the root route. Title bars, window controls, and window-drag surfaces
remain layout-bearing document leaves only so the solver can place them; their base leaves neither
paint nor handle input. After layout, each exposes a pathless `HostLayer<WindowCommand>` from its
solved window-space bounds through the same overlay adapter. Window-control gesture state belongs
to that leaf host rather than the layer: a press arms and captures, release over the same button
emits, and release elsewhere cancels.

This is the M6a popup route generalised, not a second overlay mechanism. A picker still owns its
document path and reconciled open/highlighted state, but its popup output is a `HostLayer<usize>`.
Popup, document-leaf chrome, and the root window host replay layers through the same adapter, and
picker targets are derived from the same `LayerHit` values that describe the popup. The path
belongs to the picker that consumes the typed action; it is not smuggled into the layer.

The outer host flattens and draws the document's complete nested overlay chain first, including an
open picker, then paints the root resize and drag layers. Input takes the reverse priority: the root
resize layer answers first, and only an ignored input reaches the popup or document underneath. A
picker portal exposes its popup in the next nested overlay tier, so the popup paints above
document-leaf chrome and gets the first opportunity to capture an overlapping input. A hosted
picker routes that opportunity through the canonical `Host` engine; a leaf-owned picker consumes
it locally. Neither route mirrors picker state. Within one host, layer ordering is paint order and
arbitration walks layers and hit regions in reverse. The resize layer partitions the outer strip
into four square corners and four non-overlapping sides. `Rect::contains` is half-open, so for a 4
px west edge `x < 4` resizes while a press one logical pixel inside that boundary reaches the
document control beneath it. The drag ghost owns no hit region, cursor, or event; pointer motion
only requests the repaint that moves its retained drawing.

`TextContext::shape` takes a whole `TextRoleSkin` rather than a face, a weight and a size, and that
signature is the contract. `TextRoleSkin::spacing` is letter tracking, and a signature that took
the pieces loose let a caller shape text and drop it: `render::typography::styled_text` did exactly
that, because iced's `Text` has no letter spacing to hand it to, so every string rendered through
iced ignored the tracking its skin declared — `brand` and `brand_small` at 0.3, `micro_label` and
`titlebar_text` at 0.12, `vis.meta` at 0.1, `caption` and the swatch label at 0.08. Passing the
role whole means the omission is not expressible. `color` rides along unread by shaping; that is
the price of making the role the unit.

Embedded coverage is narrower than the face count suggests: Inter and JetBrains Mono carry Latin,
Cyrillic and Greek, while Space Grotesk is Latin-only. `kithara-dark.kskin.ron` spends the Display
family on `track_title`, `title`, `deck_letter` and `brand`; a Cyrillic track title in a deck header
therefore shaped to `.notdef` under the base while the iced host's own stack still fell back
through cosmic-text, so the hole became user-visible exactly when the base took over all text.

`TextResources::new` closes it for Cyrillic and Greek by naming Inter as the collection's fallback
for those two scripts. Under AGENTS.md this is a legitimate fallback rather than a workaround: it
is the Unicode fallback mechanism itself, a user-facing capability, not a branch papering over a
broken state contract. A fallback key carries a script and a locale rather than a family, so the
registration is collection-wide — Inter and JetBrains Mono carry both scripts themselves and never
reach it, and only the Display family does. Greek is registered because Inter was measured to cover
it rather than assumed to, and a test holds that measurement; a fallback that resolved to `.notdef`
would be worse than none.

`TextResources` takes an explicit font policy. Production `Skin::resolve` enables Fontique's system
collection, while deterministic harnesses resolve a skin with the embedded-only policy. Both
policies register the Cyrillic and Greek fallbacks above. The system feature is unconditional;
targets without a Fontique platform backend receive its empty dummy collection and retain the
embedded-only behaviour.

What the production policy actually reaches is the machine's business, not this crate's, and it is
not uniform. Measured on an Apple/CoreText host against 519 scanned families: Hebrew resolves to
Lucida Grande, Arabic to Geeza Pro, Korean to Apple SD Gothic Neo, Thai to Thonburi, Devanagari to
Kohinoor Devanagari — while **Han and Japanese resolve to nothing**. That last one is an upstream
gap rather than a policy decision: `fontique-0.6.0/src/backend/coretext.rs` asks CoreText for a
fallback font, gets `PingFang SC`, and then looks that family up in its own scanned name map, which
does not contain PingFang; the lookup misses and `fallback()` returns `None`. So a CJK track title
still shapes to `.notdef` on macOS.

Selecting a face ourselves when Fontique declines would be exactly the fallback chain AGENTS.md
forbids — the mechanism is Unicode's and upstream's, and a second private path over the top of it
would hide the real defect. The tests therefore pin the contract rather than a script list: the
harness policy reaches nothing outside the catalog, the production policy paints real glyphs for
every script the machine can answer and leaves `.notdef` for the rest, and **at least one** script
outside the catalog must resolve, so the system arm cannot quietly become dead code.

Where that owner sits is transitional and is stated here so it does not become permanent by
default. Each text-drawing widget still owns one `TextContext`, so a document holds as many Parley
shaping scratch buffers and font collections as it has shaped widgets. Consolidating those
contexts needs a document-scoped owner that does not exist yet. Font-derived draw resources no
longer follow that per-widget lifetime: `TextResources` builds the ten skrifa outline collections
and scans the system collection once, then lends those resources to contexts and the iced backend.
`TextContext::from` clones Fontique's collection; its system backend and name maps are `Arc`-shared,
so a widget clone does not enumerate the machine again.

For an embedded segment, the Vello backend still builds a `FontData` per text call. A system segment
already owns the exact `FontData` Parley shaped, so Vello borrows it directly. The Masonry host now
encodes these lists into its scene, but it still supplies no measured reuse case for an exported
font cache. `TextMeasurer` therefore borrows the leaf's canonical `TextContext` and owns no second
cache or font collection.

Vello 0.6 has a known scoped-clip defect tracked as vello#1198: blend layers opened beneath
`Scene::push_clip_layer`, including those used by COLR/CPAL or bitmap color-glyph drawing, may
compose incorrectly before the outer clip is popped. Vello 0.9 does not resolve it, and the 0.6 pin
is independently required by the prospective masonry host. The backend contract remains
unrestricted: it does not replace the clip with a composition layer, narrow text to outline-only
faces, or route color text around the clip, because each would introduce a different local
rendering contract or a forbidden fallback path. Instead every Vello clip whose nested command
tree contains text with a `GlyphFace::System` segment emits one `tracing::warn!` naming
`vello#1198` and the affected region. The paint path checks only the face discriminant and never
parses font tables. All ten embedded faces are plain outlines; the Masonry host can now encode a
Vello scene, but its built-in and example text uses those embedded faces, so the affected
system-colour-font-under-clip case remains outside its shipped path. The warning is the contract
until the upstream fix lands.

`text::FontId` owns family/weight-to-face selection and the face-to-byte mapping. Its tenth value is
the embedded Lucide face, and `render::fonts` re-exports the same catalog bytes for iced host
registration. A system fallback has no `FontId`; it carries Parley's `FontData`, including the
collection index required for TTC faces. Invalid compile-time embedded font data is a fail-fast
construction error.

Text crosses the draw seam as the source string, a neutral `GlyphRun`, a transform, and a
colour. `GlyphRun` carries font size, measured width and height, and its glyphs as a sequence of
single-face `GlyphSegment`s in visual order, each naming a `GlyphFace`. It cannot name one face for
the whole run, because script fallback means one string crosses faces mid-run: a Cyrillic word
followed by a Latin one shapes
as three Parley runs over two faces, and a run that reported a single face would hand Inter's glyph
ids to Space Grotesk's outline table — wrong glyphs rather than `.notdef`. Registration records each
embedded blob's stable `Blob::id()`; shaping maps a table hit to `GlyphFace::Embedded(FontId)` and a
miss to `GlyphFace::System(FontData)`. Byte addresses are not identity because two evaluations of
an `include_bytes!` promoted constant need not share an address. Each segment also retains the
normalized variation coordinates used during shaping, so variable system faces are outlined at the
same instance whose advances Parley measured. The grouping sits on the segment rather than on each
glyph because backends bind one outline collection per face.

Backends do not shape: Vello submits each segment's positions to `draw_glyphs` under that segment's
font data. Iced canvas uses the cached static outline collection for an embedded face; for a system
face it builds a call-local `FontRef` at the carried collection index and borrows its outlines only
while constructing the canvas path. It never leaks system bytes or calls `Frame::fill_text`.

Parley 0.6, Vello 0.6, and the iced outline path use the same skrifa 0.37 crate instance. Full
positioned glyph data may cross between shaping and rendering; there is no bare-glyph-id-only
boundary. Direct skrifa use belongs only to the iced outline adapter because Parley does not expose
glyph outlines.

A render-tree adapter owns toolkit lifecycle, interaction state, and the translation from measured
bounds to widget rectangles. `DrawList`, `Backend`, `replay`, and `VelloBackend` are public so an
external shell can produce or consume the toolkit-neutral contract. The iced backend remains
crate-owned.
`Rgba`, `Pt`, `Rect`, and `Transform` are directly constructible stable value contracts.

## Layout Ownership And The Parity Harness

`render::document` owns the recursive compiled-document walk, conditional visibility, retained-host
selection, and root composition. Its public `Host` contract names no toolkit; `render::tree::render`
is the iced adapter and delegates the whole walk to that facade. `solve` owns the neutral `Length`,
`Limits`, `Padding`, `Alignment`, `Point`, and `Size` vocabulary along with main-axis distribution,
cross extent, and child offsets for expanded `Row`, `Column`, and `Slot` nodes and compiled `Split`
nodes. `render::tree::flex` translates iced constraints into that vocabulary, measures each mounted
child through iced, and translates the returned placement back into iced layout nodes. Module chrome,
native `Vis`, exact intrinsic control measurement, and the complete widget lifecycle remain
host-local through `solve::Measure`.

This boundary is shared because masonry needs the same recursive `Range` minimum distribution but
cannot express per-child minimums through its native flex protocol. Each host therefore supplies
only measurement and placement while `solve::resolve` and `solve::fluid::allocate` keep one layout
answer. Neutral draw lists remain the shared paint contract for portable controls.

### Document hosts

A document host is the fold target of `render::document::render`. The facade owns traversal,
conditional visibility, module expansion, document paths, ownership selection, popover selection,
and window composition. A host receives the already-folded neutral descriptions and owns only its
toolkit tree, measurement, placement, paint replay, and event delivery. `IcedHost` returns rebuilt
`Element<UiEvent>` values; `MasonryHost` returns a retained `MasonryNode` containing real
`WidgetPod`s. Neither host is allowed to re-walk the compiled document or retain a parallel virtual
layout tree.

The two hosts share the compiled facade, `solve` distribution, `DrawList` vocabulary,
`TextContext`, skin metrics, and the single `render::event::control_event` constructor. They differ
where their toolkits genuinely differ. iced carries compression in its `layout::Limits` and moves
the returned layout nodes directly. Masonry's `BoxConstraints` has no compression bit, so the
parent writes the complete neutral `Limits` into its private child through `AllowRawMut`, requests
layout when that side channel changes, and calls `LayoutCtx::run_layout`. Masonry rejects negative
box maxima that iced's permissive intermediate limits can represent; the adapter clamps that child
measurement constraint to a valid non-negative box before entering Masonry. Group padding remains
host-local because iced's outer `Container` fits padding to the available extent before placing its
inner flex. After `solve::resolve`, Masonry reruns every child under the exact allocated size and
calls `place_child` at the neutral offset.

`place_child` then applies Masonry's documented pixel snapping: it rounds the origin and the far
endpoint independently, and derives the stored size from those endpoints. The Masonry parity test
therefore queries the real retained `WidgetId`s and requires each stored rectangle to equal the
iced fixture rectangle under exactly that formula. It permits no epsilon and asserts the linear
part of every window transform is identity, so affine compensation cannot make a wrong layout
pass. The two builtin presets contribute 114 node comparisons across 1280x720, 960x600, and
320x240.

Masonry event delivery also stays toolkit-native. Every custom component declares one concrete
`Action`; the host maps it to the application action and erases it only inside the private
`HostAction` crossing required by Masonry's heterogeneous tree. `MasonryRoot<Action>` owns the
render root's signal callback, synchronises native layer signals, recovers the declared type, and
returns concrete values from `take_actions`. A `UiEvent` queue is not an action channel, and no
custom action is encoded into one. Built-in control events still pass through the sole
`render::event::control_event` constructor before the same private mapping. Full iced-only control
painters and `Vis` remain toolkit-local; `Vis` is deliberately not substituted with a second
visualiser here.

Pointer input crosses the seam as one `PointerInput`: stable identity, optional changed button,
host-logical point, click count, and the raw or recognised phase. `Outcome<Action>` keeps three
independent facts: an optional typed value, whether this event propagates, and whether retained
pointer ownership is unchanged, claimed, or released. Claim is accepted on `Down`; while claimed,
Masonry capture and the iced retained router keep delivering moves outside the original hit area.
`Up`, `Cancel`, terminal `DoubleClick`, or an explicit `Release` returns routing to hit testing.
`DoubleClick`, `LongPress`, and `MoveLongPress` are neutral recognised phases; a custom component
resets its gesture on terminal `DoubleClick`, may otherwise retain its own recogniser state, and may
poll an injected signal during `frame`, which is also the typed egress for an action recognised
during paint.

Open popovers are native host layers in both toolkits. Content receives input first, the full
viewport layer captures every remaining inside press, and an outside press or Escape emits the
same typed dismiss event. Placement shares the anchor/pointer origin, one-pixel frame overhang,
below-then-above flip, alignment, and viewport clamp. `MasonryState` carries the opening press
across the closed-to-open document rebuild; `MasonryRoot` updates solved anchor rectangles after
layout. Hosted Masonry subtrees reconcile one retained `Engine` over the geometry of controls whose
`InputOwner` is `Engine`; leaf-owned controls keep their local gesture and cannot duplicate that
route. Window drag, title, controls, resize edges, and the drag ghost use native layers above the
layout tree in both hosts. The outer resize layer is topmost; the ghost is paint-only and pointer
motion dirties its Masonry layer before requesting redraw.

`CustomWidget` is public because the next consumer lives outside this crate. Its associated
`Action` and `input`/`frame` methods use only the neutral interaction vocabulary above. Its `measure` method
receives public `SizeLimits` plus a borrowed public `TextMeasurer`, so an implementor uses the same
Parley faces, metrics, and tracking as the built-in text path rather than importing another shaper.
The returned `Size2` is authoritative only for a `Shrink` axis. `paint` receives the document's
resolved `Rect` and appends to the public `DrawListBuilder`; a `Fill` widget that preserves authored
aspect ratio computes its own contained rectangle and letterboxes there. The host never stretches
or transform-compensates it. `Repaint::{None, NextFrame, Continuous}` is a public declaration
because frame scheduling belongs to the embedder: Masonry paints the result of every delivered
frame, then requests another only for `Continuous`, rather than running an unconditional repaint
loop. The headless
`masonry_host` example exercises text measurement, internal letterboxing, Vello replay, continuous
frame declaration, and typed root egress without depending on lsq.

The root adapter sends a compiled `Split` weight to `Flex` as the document's original `f32`. A fluid
cell uses `Length::Fill` only to declare that it participates in distribution; the solver reads the
separate `f32` main weight, so no iced `FillPortion` conversion sits in the path. A fixed cell keeps
its declared extent and ignores the weight. Expanded `Row`, `Column`, and `Slot` children do not
supply an explicit weight and retain their existing iced `Length` semantics. The sized-child path
measures the child under the same loose limits as the removed iced container and resolves the cell
extent separately, without emitting a layout wrapper. `Split` and `Slot` pass no `Range` minimum
because their previous paths carried none. `LayoutSkin` no longer carries the scale and minimum that
drove the integer conversion; the minimum was already unreachable, because a split weight that is
not finite and strictly positive is rejected at validation.

`Range` keeps its `length_for` mapping to `Fill` or `FillPortion`, so the existing weight reaches
the solver unchanged; the render tree passes the main-axis minimum alongside it for expanded rows
and columns. Until the solver existed there was nowhere to enforce that minimum, so it was simply
discarded - a control declared `Range(min: 90)` could be laid out 33 px wide. That was a defect,
not a design, and `solve` now honours it while dividing fluid space. `Range` maximums stay
unimplemented: every `Range` in the repo declares `max: None`, so a clamp would be untested code.
Omitted `Row` and `Column` gap and padding defaults have one owner:
`SkinDoc.layout.grid_gap` and `SkinDoc.layout.grid_pad`, used by intrinsic measurement and rendering.

When the minimums cannot all be met the solver scales every one of them by a single factor. This
is a deliberate degraded mode, and AGENTS.md wants such a branch justified rather than assumed.
It is total (no panic, no negative extent, no division by a zero minimum sum), it is continuous in
the container width, and it never overflows the parent - which is what makes it preferable to
honouring minimums absolutely and letting a row spill. It is also unreachable in the shipped app:
`gui::frontend` declares a minimum window of 1080x640, and at 1080 the micro row's two
`Range(min: 90)` children have 893 px to share. The parity harness exercises the branch at 320x240
on purpose, because a library consumer carries no such window floor. If a document ever does
exhaust its minimums inside the declared floor, the document is what gets fixed.

`tests/layout_parity.rs` holds this owner to iced's prior answers. It compiles the two builtin
presets, renders them through the root adapter, resolves the tree against a headless
`iced_tiny_skia` renderer at three viewports, and pins the absolute rect tree in
`tests/fixtures/layout/*.rects` - one line per document node, keyed by its path and indented by
document depth. The walk descends the document tree and iced's layout tree together, deriving each
wrapper step from the node itself, and proves the correspondence twice: it attributes every iced
node to a document rect, a named wrapper, an opaque control's interior or named furniture and
asserts the count balances, and a second counter walks the document without consulting iced so a
subtree cannot be mistaken for furniture and vanish while the totals still add up.

Three expanded nodes carry no layout node of their own, and the walk treats them exactly as the
hosted-descriptor walker in `render/tree/host.rs` does. `Optional` is transparent - it renders as
its child and the walk passes straight through, so the child keeps the parent's path and position.
`Popover` and `Pressable` are opaque: `Anchored` returns its anchor's layout node and `mouse_area`
delegates to its content, so each is one document rect whose interior belongs to it, named by its
own control path. Both mirror `HostedControl::new`, where `Optional` recurses into the child and
`Popover`/`Pressable` are `Passive`. Diverging here would let the fixture disagree with the
descriptor inventory about the same document.

The reads fixture answers
with constant values rather than `None` so text intrinsic sizing participates; a corpus of empty
strings would measure nothing and pin nothing. Byte-identical builtin fixtures through the root flip
prove parity for the shipped weights, including final edge rounding, spacing distribution, and
cross-axis sizing. A synthetic `0.335:1.0` split at 1335 pixels separately proves the document's
fractional weights reach the solver without integer rounding. Fixtures are re-recorded only when a
document deliberately changes, in the same commit, via `KITHARA_UI_UPDATE_LAYOUT_FIXTURES`.

Reproducibility rests on one non-obvious fact. iced measures text during layout through a
process-global cosmic-text font system, not through anything this crate owns, so the harness loads
the embedded faces into `iced::advanced::graphics::text::font_system` before it lays anything out.
Without that load the skin's family names resolve to whatever the host machine happens to have
installed and the committed rects stop being reproducible.

Two residues of that global remain. Its font database still holds the machine's system faces, so a
host carrying its own face under one of the skin's family names can win the query ahead of the
embedded one, and unported iced text could still reach a system fallback face. The base shaping
path receives an embedded-policy `Skin`, and a corpus test holds every committed fixture string to
real glyphs across the embedded Display, Sans and Mono families. The remaining global behaviour is
inherent to the iced path being pinned until those text sites are ported.

### The picture parity lane

The layout fixtures above pin geometry, which is what the two hosts agree about by construction. What
they can still disagree about is the picture, and that is measured rather than reasoned about. The
gallery example photographs every page three ways: through an iced window (`KITHARA_GALLERY_CAPTURE`),
through iced with no window at all (`KITHARA_GALLERY_CAPTURE_OFFSCREEN`), and through the retained
host into a Vello scene (`KITHARA_GALLERY_CAPTURE_MASONRY`). All three rasterise on a graphics device.
Each set writes the geometry it was taken at beside its pages, and `KITHARA_GALLERY_COMPARE=<a>:<b>:<out>`
refuses to compare two sets taken at different geometry, because two hosts scaled differently can be
made to agree or disagree at will. A set inherits the geometry of a set already in its directory, so a
window set taken at the screen's scale can be answered on its own terms.

Two things the offscreen set gets from the runtime rather than from a hand-rolled layout-then-draw
pass, because a capture that skips either draws a page of container backgrounds and nothing else: the
overlay every window layer paints from is built by `UserInterface::update`, and the renderer is reset
between pages by `UserInterface::draw`. It rasterises through wgpu and not through the software
backend, because the window draws through wgpu and this set exists to stand in for it — measured, that
substitution costs 0.3-0.9% of pixels on 21 of 22 pages.

`just test ui` is the lane: one binary takes the offscreen set and the masonry set, then compares
them against `examples/gallery/parity-budget.txt`. The budget prices each page in whole percent of
differing pixels past the noise floor, a page with no line is allowed nothing, and a page over its
price or missing from a set ends the run non-zero. The floor is under one percent, where the two
engines disagree on antialiased edges and text gamma; everything above it is a control that draws
differently, or does not draw at all, under one of the hosts. The numbers are a debt: a control that
lands must lower the pages it appears on, and a number may not rise without a reason written next to
it. The window capture, `just test ui-window`, stays for a different question - whether the offscreen path
draws what a window draws - and is the only one of the three that needs a display. It is not one side
of the parity: both its sides are the same host.

### The page behind a document

A document paints its panels and leaves the rest of its rectangle bare. That bare part is the page,
and it belongs to the skin: `Ui::background` reports `skin.palette.bg`, and every host clears to it
before the scene lands. The retained window, the headless masonry capture and the iced hosts all read
it from there. Clearing to anything else - black was the retained host's first answer - shows through
wherever a document does not reach, and reads as a difference between the hosts when the difference is
really one line of host setup.

## Interaction Ownership

`interact` owns the portable pointer, wheel, key-press, key-release, and modifier vocabulary, the
gesture state machines, their pixel and time constants, and the cursor vocabulary. It imports
`draw::{Pt, Rect}` and nothing else from this crate - it does not know what a `UiEvent` is. A scalar
recognizer answers with an `Outcome<f32>` and a click answers with an `Outcome<()>`, each carrying
its capture flag. A component widens a recognizer scalar exactly once at its boundary, and
`EngineEvent::Scalar` carries `f64` from there through `ControlAction::SetScalar`; render event
publication performs no later widening or narrow round trip. The engine maps the other values into
`EngineEvent::{Activate, Crossing, Index}` without losing capture. `EngineEvent::Index` carries
the selected `usize`; the stateless segmented component derives it from the click's hit rectangle
and item count. An engine `Emission` may address one fixed child endpoint. `render::event` binds the
result to the document publisher one layer up and in the same file as the `UiEvent` it names,
including binding an index to `ControlAction::SelectIndex`. The fourth emission kind,
`Crossing(bool)`, is retained hover state:
it observes exactly one entry and one exit for a target boundary, emits nothing for motion within
the target, and never captures. Its render binding reuses `ControlAction::Drag(DragPhase::Over)`.
Routing the document event through the recognizer or engine instead would point the base at the
crate's orchestration layer, and every base peer points strictly downward: `draw` to `text`, `text`
to `skin`, `solve` to `layout::Axis`.

The retained interaction boundary explicitly names nineteen documents: `studio-deck`,
`studio-strip`, `studio-mixer`, `studio-mixer-single`, `studio-overview`,
`studio-overview-row`, `studio-overview-single`, `gallery-knobs`, `gallery-meters`,
`gallery-toggles`, `gallery-chips`, `gallery-buttons-tab`, `gallery-cells-tab`,
`gallery-faders-tab`, `gallery-library2-tab`, `gallery-tracklist-tab`, `gallery-tree-tab`,
`gallery-module-tabs`, and `gallery-nav`. A direct layout module is selected by its compiled module
ID. Expansion records each nested include root by structural address and module ID without adding a
node wrapper, so the existing render-tree shape and layout stay unchanged. The iced host at each
selected root keeps one `Engine` in widget-tree state while its descriptor snapshot is rebuilt from
the current reads on every view.
Reconciliation matches an owned resolved control path plus component kind; it refreshes
configuration and preserves recognizer state, and never retains an `InternId` across compiled UI
lifetimes. The ordinary click wave and Hero Wave have distinct descriptor identities. The Hero
descriptor refreshes its scalar drag, visible window, and wheel answers from current progress and
zoom on every view. The nineteen module IDs form a named set in the render tree; subtree contents
never silently opt another document into the engine.

Keyboard focus is a second engine slot beside pointer capture, not another interpretation of the
capture owner. It retains one resolved control path. A pointer press inside a focusable target moves
focus to that path; a press that lands on no focusable target clears it. Acquiring, transferring, or
releasing pointer capture never moves focus. `KeyPressed` and `KeyReleased` bypass the pointer holder
and route only to the focusable component at the focused path. Reconciliation retains that path while
the component still exists and remains focusable, and clears it when the path disappears instead of
retargeting by document order or component kind.

The mixer host absorbs the expanded roots of both included strips. Rendering beneath that host
propagates `InputOwner::Engine`, while only `InputOwner::Leaf` may open a host, so the nested
`studio-strip` markers cannot open second hosts. The descriptor and target walks recurse through the
already-expanded rows and columns into both strips. One mixer therefore owns one engine and one
pointer-capture slot for its crossfader, knobs, and VUs; a captured drag in one strip remains
exclusive while the pointer crosses the other strip.

The engine carries nine component shapes in one `RetainedComponent` enum. `ScalarComponent` owns
its resolved path, `Kind`, `Scalar`, and `ScalarState`; `ActivationComponent` owns a path, the
pointer hover, and the stateless `click::on_input` gesture shared by Button, Toggle, and Checkbox;
`CrossingComponent` owns the previous boundary state and preserves it across reconciliation so a
view rebuild while still inside cannot publish a second entry;
`SegmentedComponent` owns a path, item count, pointer hover, and the same stateless click gesture;
the `ContextBar` picker shape owns the focusable index selection and its open/highlighted projection;
`TextInputComponent` owns the focusable caret, selection, pointer drag, and current preedit, while
retaining only a reconciled working mirror of the document query for applying sequential edits;
`ScrollComponent` owns a path and the one mutable scroll offset, retains it across reconciliation,
and refreshes row count, row height, and viewport extent without notifying the document;
`ItemComponent` owns one list target, its document publisher, and the pressed row index;
`HeroWaveComponent` owns modifier and loop state while reusing `Scalar` for the plain drag. A narrow
`Component` trait gives the router only path, kind, event handling, cursor, capture-slot state, and
whether the component accepts keyboard focus.
The dispatch exists because activation, segmented and picker selection, and the Hero Wave have
emissions or state a scalar alone cannot express, not because controls happened to have different
names. `Kind` belongs to the retained identity, so reconciling a different component shape at the
same path rebuilds recognizer state and clears capture. One router owns the subtree's sole
pointer-capture identity and sole keyboard-focus path. The holder receives pointer input
exclusively until it releases; otherwise reverse document order chooses the topmost non-ignored
component. Key input uses only the focused path and never the capture identity. Cursor resolution
follows the pointer holder first, then the topmost non-default cursor. Hosted leaves the engine claims
use paint-only canvas programs. Engine events cross through `render::event`, so
`render::event::control_event`
remains the only production `UiEvent::Control` constructor. Button was the first activation control
whose fill, frame, and label or Lucide glyph were all painted by a toolkit-neutral base atom through
one `DrawListBuilder` instead of by iced; NavItem now owns its background, marker, icon glyph, and
label through the same seam.

The Leaf adapter owns the transient pressed visual while its click recognizer sees the pointer
gesture. The Engine adapter is paint-only and derives only idle or hovered paint from the cursor;
the required existing `Descriptor::Activation` is stateless and carries no held phase. Adding a
second pressed-state channel merely for paint would create the parallel mutable ownership this
transition forbids.

The interactive `canvas::Program` for a button, nav item, segmented control, fader, crossfader,
knob, vertical VU, stereo meter, toggle, checkbox, text input, tree, or wave is therefore not gone:
`InputOwner`
picks the paint-only variant for `InputOwner::Engine` only when the host has a matching descriptor,
and the interactive variant answers everywhere else under `InputOwner::Leaf`. An effective SVG
button or nav item is one deliberate example: it has no descriptor and remains an iced leaf until
its painting is ported. Two input paths for one control are a transition, not the design - each
disappears when its last unhosted site flips, and the pair must not grow a third reader or a second
capture slot in the meantime.

The host observes decoded input before its child. An engine emission is bound once; a captured
outcome suppresses child delivery, while an observed outcome publishes and still forwards the same
iced event unchanged. This is the drop-zone crossing contract: `Over(true)` once on entry,
`Over(false)` once on exit, no message for motion within the zone, and no capture. Cursor resolution
follows the same order: a non-default engine shape wins, otherwise the child decides. A hosted
subtree may therefore contain interactive controls and wheel-catching containers the engine does
not answer. `KeyPressed` and `KeyReleased` enter the portable vocabulary with their modifiers, and
modifier changes still reach the Hero Wave's shift state; touch does not enter this boundary. A key
consumed by the focused component is reported to iced as `Captured`. A declined key, or any key with
no focused component, remains `Ignored` even while an unrelated pointer gesture owns capture. This
preserves the app's `Delete` and `Backspace` shortcuts unless the focused control actually consumes
them. Picker arrows may repeat, while held Enter and Space presses are inert until release or focus
loss so native key repeat cannot alternate open and commit states. When engine focus is acquired,
the host unfocuses iced descendants without replaying the press; ignored character or IME events
therefore cannot edit another focused descendant behind the popup. Answered picker pointer
input is also reported as captured to iced, preventing click-through outside the hosted subtree.

Module chrome remains iced-composed during this transition: its header `Row`, panel column, footer,
and frame stack keep their existing layout ownership until the root flip removes `render/tree`.
Within that composition, the header chips, title cell, and separator lines paint through
`DrawListBuilder`. One header-sized chevron canvas shares a single painter between the interactive
`InputOwner::Leaf` program and the paint-only `InputOwner::Engine` variant. The engine-owned header
activation binds directly to `UiEvent::ToggleModule`; it is not a control action. The chrome host
owns only the header and drop boundary. A separately hosted module root keeps its existing inner
host, so an observed outer crossing can publish and forward the same event to the content engine;
the two hosts own disjoint targets and no shared gesture state.

The dual mixer adds one crossfader and an inert divider around two supported strips; the single
mixer contains one supported strip. Each strip contains knobs, a vertical VU, and a label. Each
`studio-overview-row` contains one supported wave between two inert text labels. `gallery-knobs`
contains four knobs and a label; `gallery-meters` contains a stereo meter, two vertical VUs, and a
label; `gallery-toggles` contains two toggles, two checkboxes, and a label.
`gallery-buttons-tab` contains six activation buttons and two inert text labels; its micro
play/pause cell reaches the draw seam as a Lucide font glyph. `gallery-cells-tab` is the first hosted
page with mixed interaction ownership: in document order its exact engine descriptor inventory is
two activations for cue and play, four base-painted chip activations, one segmented descriptor with
`item_count: 4`, and four activations for its two toggles and two checkboxes. Its cells, select, and
status dots remain iced-answered controls, and unanswered input continues unchanged through the
child.
`gallery-library2-tab` mounts the engine-owned `ContextBar` scope picker. Its document-level hosted
inventory starts with the text input at `library2/browser/search` and vertical scroll at
`library2/browser`, followed by the picker at `library2/context` and the track-list family at
`library2/table`. The picker is not mounted by `gallery-tree-tab`: `gallery-tree-tab` has the text
input at `tree/browser/search` and scroll at `tree/browser`. The retained row canvas, offset, and
search interaction are engine-answered.
`gallery-faders-tab` has one descriptor kind for both configurations: the default fader has an
optional drag step, the Volume fader keeps continuous drag plus its own wheel step, and the vertical
VU keeps its scalar descriptor. The page's telemetry `Scalar` is an inert readout and is not hosted.
`gallery-module-tabs` contains five activation tabs. `gallery-nav` contains eighteen activation nav
items plus an inert icon and label in its header. Each hosted `studio-deck` contains one Hero Wave
and five Lucide activation buttons, while its Slot and labels are passive and its tempo row remains
an iced wheel surface. The Hero component preserves shift-loop child emissions, child-addressed
wheel zoom, and the plain scalar drag.

A `Scalar` carries a `Track` that says how a position becomes a value, and the split that matters is
relative against absolute. A relative track counts travel from the press, so the press only arms the
gesture and publishes nothing - a knob that jumped to the pointer would lose its current value on
the first touch. An absolute track reads the position itself, so the press seeks straight there,
which is what a fader thumb and a VU have to do. `HorizontalClick` is the one absolute track that
seeks without arming: the mini-wave without beats is a seek surface, not a drag surface, and leaving
the pointer free is why the press does not capture the moves that follow.

The recognizer and iced rectangle remain `f32`, so an absolute track computes the same pointer ratio
as iced. `ScalarComponent` widens that ratio before any value arithmetic. Its optional `f64` drag
step applies only to pointer press and move outcomes; wheel values retain the recognizer's separate
wheel policy. Quantizing after widening is required for pixel-exact iced parity: quantizing the
ratio in `f32` moved a half-step boundary by one rail pixel before the result ever reached the
published `f64` field.

Three properties hold across every track. The press decides `active` before computing, so a press
whose computation yields nothing still arms the gesture that follows. A degenerate extent publishes
nothing rather than a clamped zero - a control laid out to zero pixels has no position to report,
and `0.0` would be a value the user never asked for. The move arm never re-tests the bounds, so a
drag survives the pointer leaving the control and clamps at the end of travel instead of stopping.

Only `HorizontalPixels` leaves the unit interval, because a column width is a width: it floors at a
minimum and has no ceiling. `RelativeHorizontal` subtracts rather than adds, because the mini-wave
moves its content under a fixed playhead, so pulling right walks the position back.

`Outcome<T>` carries what the gesture produced and whether it took the pointer, and those are two
independent facts rather than one. `set` publishes and captures; `observed` publishes without
capturing, which is what makes `ItemDrag` work at all - the row underneath keeps its own click while
the drag overlay watches the same gesture, and that non-capture is the pinned regression. An engine
`Emission` retains the complete `Outcome<EngineEvent>` plus an optional child endpoint: a scalar
press may capture without emitting a value, activation emits once without holding the router slot,
segmented selection emits one index without holding it, and the Hero Wave emits loop and zoom values
under its own children. `Component::captures_pointer` alone decides whether the router retains an
owner. A scroll wheel that moves is a consumed event and the host reports iced `Captured` even
though scrolling retains no pointer slot; at a directional boundary its outcome is `Ignored` and
the unchanged wheel continues to the child and outward. Other consumed-without-retained-capture
and observed outcomes keep their established host status. Only `render::event` turns an engine
emission into an `Action`, routing parent scalars, activation,
and index selection through their existing publishers and child scalars through `scalar_child`.

`ItemDrag` is one value rather than the config-plus-state pair the other recognizers use, because it
has nothing to configure: the 4 px threshold is its own constant and the item's identity belongs to
whoever owns the list. It answers `DragEvent::{Started, Dropped}` and `render::event::drag`
substitutes the index, which is why the base never learns what a row is.

Exactly one file, `interact::iced`, names a toolkit. It translates an `iced::Event` into an `Input`,
including portable `KeyPressed` and `KeyReleased` values with modifiers, builds a `Hit` from bounds
and a cursor, and converts a `CursorShape` into `mouse::Interaction`. Key identity therefore enters
the engine without bringing iced with it.
`Hit` is constructed separately from event translation on purpose: `mouse_interaction` receives no
event, so a `Hit` that only fell out of an event would grow a second, unwritten hit-test path.
`Hit::at` and `Hit::inside` are different questions - a gesture already under way tracks the pointer
past the edge, while a gesture starting needs it inside - and `Rect::contains` is half-open to match
`iced::Rectangle::contains`, which makes an `at`-based test bit-equal to `Cursor::is_over`.

`Input::PointerMoved { at }` is a third question and not a substitute for either. It is what the host
says about the pointer, reported even while a widget is told its cursor is unavailable, but it is
only comparable against itself. A recognizer measuring travel reads it - `ItemDrag` must, because
its pinned contract is a drag that starts after the pointer has left the row, with no cursor at all.
A recognizer normalizing against an area must read `Hit::at` instead, because that is the position
already expressed in the area's space; feeding it the event position would be an unnoticed
coordinate-space bug the moment a control is not at the window origin.

`CursorShape` deliberately derives no ordering. `mouse::Interaction` derives `Ord` and the render
tree `.max()`-merges it in `render::tree::flex` and `widgets::anchored`; those merges stay on iced's
type and convert only at each `mouse_interaction` return.

`interact` is gated on `render` although its core names no toolkit, and that is a cost rather than a
contract. Its consumers are all `render`-gated, so under a `vello-backend`-only build - which
`cargo hack --feature-powerset` really compiles - every item would be dead code. A separate `engine`
feature would buy nothing: the `dead_exports` check treats only `target_os`/`target_arch` as a gate,
so a feature offers no exemption from the rule that actually governs this code, and `just check clippy` runs without `--all-features`, which would make the module a permanent lint blind spot. The
gate splits the day a second host routes input, exactly as `backends` already splits.

Two wheel policies coexist and must not be unified: `Scalar` accumulates 20 px of trackpad travel
per step, while `Stepper` treats any pixel delta as one detent and debounces at 200 ms. One asks how
many steps have accumulated, the other asks whether one has gone by yet - a flick arrives as a long
tail of shrinking deltas, and every one of them would be its own detent without the window. They
share only the delta decoder.

Time enters a recognizer as a parameter rather than being read inside it. That is what makes the
double-click window testable at all, and the window is `[0 ms, 301 ms)` rather than 300 because
`as_millis` truncates - a `Duration` constant compared with `<=` would silently narrow it.

`widgets::interaction::behavior` is gone entirely, and with it the 0.90 similarity pair
`ScalarDragState` formed with `interact::ScalarState`. Six tracks now share one state machine
instead of two machines sharing a field bag: press, move while active, release, plus the two
opt-ins. Every pointer gesture in the crate is a recognizer, including the stepping surface: what is left
under `widgets::interaction` is a canvas that draws nothing and forwards, which is the shape every
ported control now has.

`render::event::control_event` is the only place production builds a `UiEvent::Control`. That is
grep-provable and meant to stay so: the remaining literals live in tests, where a pin should spell
out the event it expects rather than call the constructor it is checking. The rule is not tidiness -
a binding rule, an address scheme or an emission discipline needs one site to attach to, and
fifteen literals kept in step by review is not one.

A recognizer pin and a wiring pin hold different halves and neither substitutes for the other. The
recognizer pins own the arithmetic and the capture rule against `Outcome`, where no path exists. The
wiring pins own the step the port could otherwise drop silently: that a canvas hands the publisher
its *own* path, and the row overlay its own index. There is one per publisher - `scalar` on the VU,
`activate` on the toggle, `drag` on the row overlay, `step` on the wheel surface - because that is
the smallest set that covers every way a gesture becomes a `UiEvent`.

`recognizers::click` is a free function rather than a type, following `recognizers::wheel`. A press
that lands is the whole gesture, so there is no state, and the only thing that looked like
configuration - the cursor shape - already belongs to `Hover`. A struct here would have carried
nothing and its `on_input` would not have read `self`, which is what `clippy::unused_self` says out
loud.

## Text Control Ownership

The `Text` control is the first control whose measurement and painting both belong to the base.
`atoms::text` owns the pair: it shapes through `TextContext` and either reports an intrinsic size
or emits one `DrawCmd::Text`. `widgets::text` owns only the iced side - it resolves the style,
colour and content, then hosts the atom as a leaf `Widget` whose `layout` returns the base's
intrinsic and whose `draw` replays the command list through `IcedBackend`. It reports
`Shrink` by `Fill`, which is what the `container` it replaced reported.

The two shaping calls differ on purpose and are not a cache miss to be repaired. `layout` shapes
unbounded, because an intrinsic is what the control asks its parent for; `draw` shapes against the
width it was given, so a squeezed box breaks lines instead of overflowing. A single-slot memo keyed
on the query cannot serve both, and a memo that never hits is worse than none - if shaping ever
shows up in a frame budget, the fix is a measured one with a `kithara-devtools` number behind it.

Porting it moved rects, and that was the point: `transport/tempo-label` went 24.000 -> 28.800 and
`transport/stream-label` 96.000 -> 115.200, exactly the `chars x spacing x size` the skin declared
and iced dropped. The residual against cosmic-text is zero - both engines agree on JetBrains Mono's
advance - so the whole delta is tracking.

`render::typography::styled_text` still serves `atoms::design::cell`, `atoms::design::swatch` and
`widgets::window::title`. Those are unported sites, not a fallback: they take the iced path
entirely, including losing their tracking, until their own wave moves them. Nothing chooses between
the two at runtime.

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
- A menu glyph is the one control whose declared size names one axis only: drawn as a text glyph,
  its box is the icon size wide and a line box tall (`1.3` times the size, the iced default), so a
  square declaration overflows. `size::icon_cell` fixes the width and leaves the height to the row.
- A container declaring no `size` renders `Fill` on both axes, mapped in
  `render/tree/geometry.rs::content_size`; a stack of unsized rows therefore divides its parent
  among them, and a row that should hug its content says `h: Shrink`.
- `Dim::Shrink` is the one rule the document layer cannot compose: the toolkit measures the content,
  so `Bounds` treats it as an open axis and `Dim::from(Bounds)` never produces it. A shrunk node
  must carry `Shrink` to its own children - `content_size` passes it to the container, its frame
  overlay and its fill, because the first `Fill` inside a shrunk box claims the whole row.
  Text measures its glyphs and takes alignment from that wrapper; a readout drawing its own framed
  cell keeps filling the box the document gave it.
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

### Retained button and navigation painting

`atoms::button::Button` owns button painting. It emits the background, either the uniform rounded
border or transport seam strips, and the centred label or Lucide glyph into one retained list; both
the interactive Leaf canvas and paint-only Engine canvas replay that list. An icon reaches the atom
as `render::Mark`: a Lucide glyph crosses as text because its source is a glyph in the tenth
embedded face, and authored art crosses as `Geom::Path`, read from its document by `draw::outline`
into a unit-square `Outline` that `Outline::placed` sizes to the icon box. Both halves stay inside
the draw list, so neither host reaches a toolkit widget for an icon and no capability predicate
routes around one. `MicroPrimary` retains its existing forced Play/Pause glyph and therefore
ignores a declared icon.

`atoms::nav_item::NavItem` likewise owns one retained painting: the selected background, marker
rectangle, icon mark, and mono label. Its canvas remains `Fill` wide and fixes only
`skin.nav.item_height`, so shaping the two glyph runs never participates in intrinsic layout.
`TabLarge` is the first base-painted activation control whose size comes from its own text.
`atoms::tab::TabLarge` shapes the label through `TextContext`; that shaped width is the measurement,
and horizontal padding produces the control width while the skin supplies its fixed height. The
custom iced widget performs that measurement in `layout` and replays the atom's label and
active-only underline in `draw`, so leaf and hosted tabs keep the same rectangle. Its shaping
scratch remains per-widget; moving it to a document-scoped owner is still separate debt. With no
sizing wrapper, the retained host maps the tab target from the widget's own layout bounds; wrapped
controls continue to map the child bounds inside their declared-size container.

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
- A subtree's intrinsic size is a function of the node and the visibility snapshot, constant when no
  optional block sits below it. `CompiledNode` records that at compile time in `blocks`, and
  `render/tree/size.rs::node_size` returns the precomputed `compile::compiled_node_size` for such
  subtrees instead of walking them once a frame - memoization, not a fallback path. A layout
  `Module` declaring its own `size` records `blocks: false` and never re-walks.
- `CompiledUi.size` is that function evaluated with every block visible (`size::VISIBLE`).
  `size::compute_size` takes the snapshot as a `size::Hidden` predicate over `BlockSpec`, so
  visibility-aware sizing stays toolkit-independent and available in non-render and wasm builds.

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

A portable `Button` inside a `Pressable` resolves by traversal rather than by nesting. Its shared
click recognizer publishes and captures the left press before `mouse_area` sees it, so the
`Pressable` stays silent and the release publishes nothing. The explicit-SVG legacy iced button
still captures the press and publishes on release; that earlier capture likewise keeps the wrapper
silent.

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
  every `ButtonPressed`, and `latch` consumes that record on the false-to-true edge of the open flag,
  so moving over an open surface never drags it and every open takes its own press. With no banked
  press it consumes `None` and places at the anchor, the shape a keyboard-driven open would take.
  `Widget::overlay` carries the latched point through the same translation as the anchor rectangle.
  The flip frame cannot read the live cursor: `iced_runtime` builds the overlay before updating the
  base tree and hands that tree `Cursor::Unavailable` while the overlay claims any interaction.
- The surface claims the cursor over itself and nowhere else: claiming wider would cost the whole
  base tree its cursor for that pass, the signal deciding whether the pointer belongs to the overlay.
- `SkinDoc.pop` solely owns the pop chrome - background, frame, gold cap, shadow - and `Anchored`
  paints all four; markup declares only content, because redeclaring the chrome in a document would
  create a second owner. Frame and cap draw outward of the content column, so a 298 px column yields
  a 300 px surface exceeding the content height by twice the frame plus the cap.
- `Anchored` holds exactly two children: `render/tree/node.rs` hands it `iced::widget::Space` for a
  closed popover's content rather than dropping the child, so the two-entry split in
  `Widget::overlay` and the single layout child in the overlay's `draw` and `update` are total
  patterns, not fallback branches. `Tree::diff_children` rebuilds the content state on each open.
  The overlay's layout node is the whole surface with the content column as its single child, so
  `layout.bounds()` means "the popover" everywhere and a press on the frame belongs to the menu.
- `Widget::overlay` always wraps in `overlay::Group`, including a group of one:
  `overlay::Nested::draw` bounds the layer by the top-level element, `Group::layout` expands that
  element to the viewport, and a bare `Anchored` element is exactly the surface, so the shadow drawn
  below and blurred outward otherwise falls outside it. `Anchored` declares no overlay index and
  takes iced's default.
- Dismissal fires only on a mouse press with the cursor outside the surface, or on Escape - never on
  a release, a move or a scroll, since the opening press is captured while the popover is still
  closed and its release lands on the fresh overlay over the anchor. The widget publishes
  `ControlAction::Activate` on the `Popover`'s own path, never the anchor's, and captures the event
  so the anchor's press cannot also fire; the host's popover handler is therefore set-false and
  never a toggle, the anchor's press stays the only toggle, and outside press and Escape are
  idempotent.

### Engine-owned context-bar picker

The engine-owned `ContextBar` scope picker is the crate's second overlay-producing widget, but it
does not introduce a neutral layer contract. Its base `DrawList` replays in the ordinary canvas and
therefore remains under the subtree's clip. When the picker is open, its popup `DrawList` replays in
a fresh iced overlay frame after that subtree draw and clip have completed. The two retained lists
are separate adapter outputs; no `DrawCmd` escapes a `DrawCmd::Clip`, and the code does not assemble
a synthetic combined command tree. The Leaf widget state contains its local engine; the Engine
variant contains only the host-projected picker snapshot, so there is no dormant second mutable
engine. An answered base-tree key invalidates layout while the popup is open, including when its
retained snapshot is unchanged, because iced clears its cached overlay on base capture and rebuilds
it only during revalidation. General retained layering and cross-backend base/popup ordering belong
to the M7 root flip.

## Shipped App Menu

`assets/modules/app-menu.kmodule.ron` and the row templates it includes from
`assets/modules/app-menu/` are shipped assets that `builtin::resolver()` deliberately does not
answer for: their window-manager endpoints - window list, per-window module flags, saved layouts -
are host state no crate owns, so the documents must not become canonical preset surface the studio
can resolve. Exactly one copy of each exists and every consumer reaches it with `include_str!`.
The window row, module-grid cell, saved-layout row, preference toggle and hint-reporting toggle are
one template each (`window-row`, `module-cell`, `layout-row`, `toggle-row`, `hint-row`), taken as
often as the menu needs through `Include`. Each instance's control paths are
`app-menu/<include id>/<node id>`, so the template's own ids stay plain.

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
the row that holds them, so the same controls sit in a 26 px gallery header and in the studio's
42 px bar; the document declares the background. `WindowDrag` is the bare drag surface a bar without
a title needs, `TitleBar` the same surface with a label. Both emit on press, not on release - a
window drag only takes effect while the button is still held. Their glyphs are canvas strokes drawn
to the skin's icon size.

`TitleBar` and `WindowControls` are portable, binding-free controls emitting typed
`UiEvent::Window(WindowCommand)` values; the host owning the native window ID executes drag,
minimize, maximize and close. `kithara-ui` owns their declarative schema and skin-driven
presentation, never native window state.

## Track List Column Ownership

`TrackList` owns an ordered typed column list and requires `Title` during compilation. Its optional
document field is a `Param<Vec<TrackColumn>>`, so an including document may pass it as an argument
(`columns: "$columns"` against `with: { "columns": "[Deck, Title, Artist]" }`); `Param` is untagged,
so a literal `columns: [Title, Artist, Time]` parses as before. Validation reads the resolved list:
`ControlSite` carries it already substituted beside the substituted `read` and `write`, so routing
columns through a parameter cannot slip past the `Title` requirement. This lets one
`library.kmodule.ron` serve both the built-in player preset and the app studio.

The renderer owns table geometry and cell presentation but not column visibility: with a
`columns_state` binding present the host may expose Bool reads at
`<binding-id>.<column endpoint_name>`, and a missing derived endpoint means that column is visible.
This keeps one declarative column inventory while letting library, playlist and set-queue hosts
apply presets without renderer-owned state.

The `Deck` column marks assignment and does not offer it: it shows the letters the host put on the
row, and nothing when there are none. One row-item owner arms selection on press and gives the same
index to `ItemDrag`. A plain release over that row emits `SelectIndex`; pulling past the 4 px drag
threshold suppresses selection, emits `ControlAction::Drag(DragPhase::Start(index))` on the control
path, and makes the release emit `DragPhase::Drop`. The recognizer captures no event, so whatever
the drag is released over sees the same release. Reconciliation cancels a held index if its row has
disappeared.

Column widths are host-owned through Scalar reads at
`<binding-id>.width.<column endpoint_name>` and `SetScalar` controls emitted at
`<track-list-path>/width/<column endpoint_name>`; a missing width read uses the skin default. The
renderer retains only canvas drag state, clamps resizable fixed columns to the skin minimum, and
keeps the required Title column flexible with its skin-owned minimum.

The retained painter draws the header, rows, cells, dividers, footer, and both scrollbar indicators
from that resolved column layout. Input geometry remains independent of paint geometry: each
resizable edge exposes the skin's wider `divider_hit_width` rectangle to the engine while the
painter emits only the centered `divider_width` rectangle. The distinction is intentional: a
usable resize band must not make the rule look thick, and the thin rule must not shrink the band
that can start `SetScalar` drag input. Completely clipped bands expose no new hit target; a divider
already captured before a relayout retains a zero-area watcher only until release, so its scalar
gesture cannot strand engine capture after the painted edge moves offscreen.

For a hosted list, the engine retains one row-item owner, column dividers, and vertical scroll under
the document's canonical track-list path. It also retains a sibling horizontal scroll at
`<track-list-path>/scroll-x` only when the resolved columns overflow the laid-out viewport. Target
order is outer horizontal, inner vertical, one row watcher, then the visible divider bands; reverse
routing therefore tries the most specific input first. The watcher derives its index and clipped
row hit from the current cursor, but remains as a zero-area target away from a row so an offscreen
release can still publish `DragPhase::Drop` for the index held at press. A visible vertical
scrollbar reserves its painted rail and trailing margin as one non-row interaction lane. Both
canonical engine offsets are synchronized into the canvas's two `ScrollState` owners, along with
the pressed row; there are no parallel offset fields. A leaf-owned list uses the same recognizers
and scroll state locally, never a second hosted owner.

## Browser Tree Ownership

`Tree` reads a borrowed flat row slice whose depth, branch state, selection and presentation flags
are host-owned. The renderer never mutates or filters that state; activating any visible row emits
`ControlAction::SelectIndex` on the control path, and the host decides whether that index toggles a
branch or selects a leaf. `TreeSkin` owns search, row, indentation, panel and context-bar metrics.
`ContextBar` keeps breadcrumb text read-only; optional scope items use a separate Scalar read
binding and emit `SelectIndex` on the control path so scope state stays host-owned.
`validate::check_context_scope` requires `scope_items`, the scope read and the write to appear
together or not at all.

The search combines two ownership domains without merging them. The query is document-owned: each
ordinary text edit and each native IME commit emits exactly one `UiEvent::LibraryQuery` containing
the complete resulting query, and the next view reconciles that document value back into the
component. The engine owns only per-control interaction state: caret, selection, pointer drag, and
preedit. Moving the caret, extending the selection, or replacing or closing a preedit changes the
paint projection and requests a redraw without emitting a `UiEvent`. Preedit is composition, not
query text; only `InputMethod::Commit` applies it to the query and publishes. The component's query
copy is a reconciled edit mirror for consecutive input packets, not a second canonical read.

Text shaping and caret measurement come from the same Parley layout at UTF-8 grapheme boundaries.
The painter uses local canvas coordinates, while the engine target carries iced's absolute
window-client layout rectangle in logical pixels. Adding the local caret offset to that target
origin produces the absolute logical rectangle passed through `Shell::request_input_method` on
each redraw. The Leaf wrapper asks its local engine; an Engine-owned tree is answered by its outer
host. The current preedit and its byte-range selection travel with that request, so `iced_winit`
paints its over-the-spot text, selection, and underline at the reported caret. Composition is
therefore answered rather than dropped, and the canvas does not paint a second preedit copy.

Rows paint through `DrawListBuilder` inside one viewport clip. `InputOwner::Leaf` selects the
interactive canvas program; `InputOwner::Engine` selects the paint-only program and receives a
derived offset snapshot after host reconciliation and layout. The retained `ScrollComponent` is
the canonical mutable owner for a hosted path, while the leaf program owns the same `ScrollState`
only where no engine exists. Neither path sends offset changes through `UiEvent`; only row
activation crosses the existing index publisher. The painter shapes and retains only rows that
intersect the viewport, including partially visible boundary rows, while the clip remains the
overflow contract. The solid scrollbar is an offset indicator in this wheel slice: its lane is
excluded from row activation, but rail/thumb dragging and touch panning are not input contracts of
M5a.

Wheel arbitration is axis-aware, directional, and consume-or-observe. Portable wheel packets keep
both horizontal and vertical deltas and whether the host reported lines or pixels; every retained
scroll declares the single axis it owns. In reverse document order, the innermost viewport under
the pointer consumes only a non-zero delta on its own axis that actually changes its clamped
offset. A wrong-axis delta or travel beyond either boundary returns `Ignored`, so routing continues
to the next outer engine target and then unchanged to an unported iced ancestor. Consequently a
movable vertical track-list body passes a horizontal wheel to its conditional horizontal parent;
at the bottom a further downward wheel is ignored while an upward wheel is still consumed, and the
same two-direction boundary rule holds for the horizontal axis. Search keyboard and IME input route
only to the focused text-input component; with focus elsewhere they remain ignored and can continue
to the host shortcut layer.

## Application Consumer

The `kithara-app` GUI studio is the production consumer: it embeds its own layout and module
documents, implements `EndpointRegistry` (`gui/studio_ui/endpoints.rs`) and `Reads` over an address
tree (`gui/studio_reads/`), and maps `UiEvent` to app messages (`gui/studio_ui/events.rs`). Builtin
module docs under `assets/modules/` remain the canonical presets consumed by the gallery modules
page.

## Masonry Host Exports

`MasonryHost::{with_state, with_custom}` and `MasonryRoot::{take_actions,
take_platform_signals}` are the public contract a Masonry consumer drives: install retained state,
mount custom content at a document path, then drain typed actions and platform signals each frame.
`examples/masonry_host.rs` is their production caller and exercises all four end to end; the M9 lsq
migration is the second consumer.

Those four are why `dead_exports` stopped classifying `TargetKind::Example` as test-like. An example
is shipped code demonstrating a public API, `cargo --all-targets` builds it, and
`platform_layer_hygiene` already scanned the same files as production — so counting an example's
references as test-only made an export that only an example drives look dead. Removing `Example`
from that classification cleared exactly these four findings and introduced none anywhere else in
the workspace.
