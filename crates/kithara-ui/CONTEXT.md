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

## Visualizer Ownership

`Vis` reads the host-owned preset as a Scalar and emits `SelectIndex` for preset changes. Its
master reaction level is derived from the `player.output.levels` Stereo snapshot as
`max(left, right) * volume`; the shader does not retain audio state. `SkinDoc.vis` owns the module
chrome and canvas metrics, while the embedded WGSL asset owns the three fixed render presets.

The render-only shader program stores an `Instant` per widget to derive animation time. A host
must keep requesting frames while the visualizer is visible; the gallery does so with its active
VIS-tab subscription. The shader implementation remains behind the `render` feature, so the
non-render wasm schema lane does not depend on wgpu or wall-clock state.

## Typed Control Schema

Each supported control is a structural `ControlNode` enum variant. RON deserialization owns field
validation, so the document layer has no string control discriminator, property map, or property
kind catalog. Common control fields are repeated in the serde variants because RON flattening is
not part of the schema contract.

`validate::value_kinds` is the single owner of control read/write endpoint kinds. Intrinsic sizes
are selected exhaustively from `ControlSpec` and the supplied `SkinDoc` by
`size::control_size`; this remains available in non-render and wasm builds. Renderers match
`ControlSpec` directly and do not resolve a runtime control catalog.

## Module Chrome And Collapse Ownership

`ModuleDoc` owns optional shell labels, static assign labels, and footer binding plus a typed
`ChromeStyle`. `Frame` is the serde default so existing documents retain the original frame and
corner ticks; `Plain` renders only module content; `Full` adds the skin-owned 12e header,
separators, and footer. Assign labels render in the Full header immediately before its chevron.

Each layout module instance owns which outer frame sides and corner ticks are rendered. These
per-instance flags let adjacent modules yield their shared edges to the layout grid, while the skin
remains the owner of frame and corner thickness, size, offset, and color.

Collapse state remains host-owned. A Full module reads `Bool` from
`ui.module.<module-doc-id>.collapsed`; an absent value means expanded. Header activation emits
`UiEvent::ToggleModule(<module-doc-id>)`. The renderer does not retain or mutate collapse state,
and Frame or Plain modules ignore that endpoint.

## Window Chrome Ownership

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

## Detached application consumer (2026-07-20)

By owner decision the application was reverted to its pre-modular GUI and PR #117 ships
`kithara-ui` alone. Until the app integration returns, the crate's public surface
(`compile`, `EndpointDesc::with_scope`, `fonts::FONT_BYTES`) is exercised only by the
gallery example, which the `dead_exports` scan classifies as testish; those three names
are exempted in `.config/arch/thresholds.toml` with the same rationale. Builtin module
docs under `assets/modules/` remain the canonical presets and are consumed by the gallery
modules page; nothing outside this crate reads them right now.
