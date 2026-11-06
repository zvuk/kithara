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

## Integration

[Crate contracts](https://github.com/zvuk/kithara/wiki/kithara-ui).
