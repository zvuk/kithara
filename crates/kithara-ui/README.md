<div align="center">

<img src="https://raw.githubusercontent.com/zvuk/kithara/main/logo.svg" alt="kithara" width="300">

</div>

<div align="center">

[![crates.io](https://img.shields.io/crates/v/kithara-ui.svg)](https://crates.io/crates/kithara-ui)
[![docs.rs](https://docs.rs/kithara-ui/badge.svg)](https://docs.rs/kithara-ui)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](https://github.com/zvuk/kithara/blob/main/LICENSE-MIT)

</div>

# kithara-ui

Serializable modular UI model for kithara: skin, layout, and module documents, registries, and a
compiler producing a normalized UI tree for renderers.

- Documents are RON with a `schema`/`version` envelope: `*.klayout.ron` describes a layout
  preset (recursive split tree of module instances), `*.kmodule.ron` describes a reusable
  module (control tree with nested includes, slots, and parameter bindings), and `*.kskin.ron`
  describes the complete palette, typography, geometry, and control metrics.
- Controls deserialize as typed `ControlNode` variants with typed style and format fields;
  bindings reference namespaced engine endpoints resolved through a typed registry at load time.
- `compile` takes the complete skin document, resolves includes (with cycle detection and
  limits), substitutes `$parameters`, validates the whole graph, and returns a `CompiledUi` —
  or a typed error with the exact document path.
- The default crate is GUI-toolkit independent and wasm-compatible; renderers consume
  `CompiledUi`. The optional `render` feature converts the skin to iced-facing colors and adds
  shared fonts, icons, events, and read-model contracts. Built-in presets and the complete
  default skin live in `assets/`.
- A skin can register its own TrueType/OpenType faces with
  `fonts: {Sans: ["fonts/regular.ttf", "fonts/bold.ttf"]}`. Paths are relative to
  the declaring skin; a variable font can supply all weights in one file. The
  declared families use these bytes for both measurement and painting without
  replacing another skin's embedded fonts.
- `render::document` is the toolkit-neutral document fold. The iced render tree implements it for
  the existing app, while the optional `masonry` feature mounts the same compiled document and
  neutral solver into retained Masonry widgets with a public custom-content seam, typed actions,
  portable pointer capture, and native popover/window layers.
- Content an application installs with `MasonryHost::with_custom` hears every pointer under its
  own id, button and click count, and is hit in the local space it was painted in under any
  object pose with an inverse; a pose without one is refused, and a gesture it held is
  cancelled. It paints only when told: `MasonryRoot::invalidate(path)` schedules one redraw
  for state the application keeps, `CustomWidget::repaint` keeps frames coming only while it
  asks, and a window drives `MasonryRoot::needs_frame` and `complete_frame` the way the window
  runner does. Masonry holds one pointer capture at a time, so a second pointer's release ends
  the first one's capture too; the widget tells its pointers apart by id.

## Integration

A skin supplies caller-owned Lottie JSON through its ordinary resolver:

```ron
(schema: "kithara.skin", version: 1, id: "caller",
 artworks: {"caller": "art/picture.json"})
```

`parse_skin_over`/`load_skin` retain the declaring origin through inheritance and
overlays. `Skin::resolve_with_font_policy` loads and admits each declared artwork
once; cloned skins share it. With `render` enabled, `compile` checks required
`Lottie.artwork` and present `active_artwork` names after parameter substitution,
returning `UiDocError::UnknownArtwork` with the declaring document and control
path. An absent optional active artwork remains absent, and direct optional
`Skin::artwork` lookup retains its `Option` contract. The resolved resource is
replayed by both hosts without source reads or JSON parsing during frames.

The Iced drawing adapter preserves linear interpolation, coincident stops and
premultiplied opacity with native vertex-colour meshes. Geometry is prepared
before caching, so unchanged controls reuse it without retessellation.

[Crate contracts](https://github.com/zvuk/kithara/wiki/kithara-ui).
