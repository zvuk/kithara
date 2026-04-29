# kithara-probe-macros

Proc-macros backing `kithara_probes`:

- **`#[kithara::probe]`** — wraps a function with USDT probe firing
  plus a paired `tracing::event!` mirror. Provider name and tracing
  target are derived from the consumer crate's `CARGO_PKG_NAME` (with
  `-` → `_`), so a probe declared inside `kithara-stream` becomes the
  USDT probe `kithara_stream:<fn_name>` and the tracing target
  `kithara_stream_probe`.
- **`#[derive(Probe)]`** — generates a `kithara_probes::Probe` impl
  for value-type payloads. Each named field becomes a `u64` USDT slot
  and a paired tracing field.

## Generated layers

The attribute macro emits three independent layers:

1. **Function body** — never gated. Original code runs on every target
   / feature combination.
2. **USDT block** — gated by
   `cfg(all(feature = "usdt-probes", not(target_arch = "wasm32")))`.
   `usdt::provider!` requires inline asm, so wasm32 cannot link it.
3. **`tracing::event!`** — gated by `cfg(feature = "usdt-probes")`
   only. Works on every architecture, including wasm32, so
   `probe_capture` integration tests stay observable everywhere.

## Constraints

- Each annotated function must declare arguments whose types implement
  `kithara_probes::IntoProbeArg`.
- Probe-function names must be unique per crate — USDT addresses
  probes as `provider:probe`, and a name collision causes a linker
  symbol conflict.
- Workspace lint policy normally forbids non-entry items in `lib.rs`;
  this crate is whitelisted in `.config/ast-grep/style.no-items-in-lib-or-mod-rs.yml`
  because proc-macro entries (`#[proc_macro_attribute]`,
  `#[proc_macro_derive]`) must live in the crate root by Cargo
  convention.
