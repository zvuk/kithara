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
