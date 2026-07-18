# kithara-ui

Serializable modular UI model for kithara: layout and module documents, registries, and a
compiler producing a normalized UI tree for renderers.

- Documents are RON with a `schema`/`version` envelope: `*.klayout.ron` describes a layout
  preset (recursive split tree of module instances), `*.kmodule.ron` describes a reusable
  module (control tree with nested includes, slots, and parameter bindings).
- Controls reference a code-owned control catalog; bindings reference namespaced engine
  endpoints resolved through a typed registry at load time.
- `compile` resolves includes (with cycle detection and limits), substitutes `$parameters`,
  validates the whole graph, and returns a `CompiledUi` — or a typed error with the exact
  document path.
- The crate is GUI-toolkit independent and wasm-compatible; renderers (such as the iced GUI
  in `kithara-app`) consume `CompiledUi`. Built-in presets live in `assets/`.
