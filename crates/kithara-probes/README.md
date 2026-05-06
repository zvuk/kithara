# kithara-probes

Runtime support for the `#[kithara::probe]` attribute macro.

## What it provides

- **`IntoProbeArg`** — trait that domain types implement so the macro
  can blindly convert any annotated argument to the wire `u64` USDT
  takes. Stock impls cover primitive ints/bool, `Duration`, `&Url`,
  and the project's domain enums (`RequestId`, `RequestPriority`,
  `CancelReason`).
- **`Probe`** — trait emitted by `#[derive(kithara_probes::Probe)]` for
  value-type probe payloads. Each named field becomes a `u64` USDT
  slot and a paired `tracing::event!` field.
- **`register_probes`** — idempotent global USDT registration. Safe to
  call from any number of init paths.
- **`kithara`** namespace — re-export so consumers write
  `use kithara_probes::kithara;` and `#[kithara::probe]`.

## Wire format

All probe args encode as `u64`. Domain enums use stable discriminants
(see individual `IntoProbeArg` impls). `Option<T>` uses `u64::MAX` as
the `None` sentinel — reserve that value if your `T` could legitimately
produce it.

## Activation

`#[kithara::probe]` emits no-op code unless the *consumer crate* (the
one carrying the annotated function) declares a feature named
`usdt-probes` and it is enabled at build time. This crate's own
`usdt-probes` feature toggles whether `register_probes` is a real call
or a no-op stub.

USDT probes require inline asm and are gated to native targets only;
the `tracing::event!` mirror runs on every architecture (including
wasm32) so `kithara_test_utils::probe_capture` can observe events from
integration tests regardless of platform.

## `usdt_wire.rs` carve-out

`src/usdt_wire.rs` is the single point in the workspace where USDT
inline asm lives. Two structural lint suppressions are documented
inline and globbed into `.config/ast-grep/rust.no-lint-suppression.yml`:

- `#[expect(unsafe_code, ...)]` on the `prov` module — the
  `usdt::provider!` proc-macro from the external `usdt` crate expands
  to inline asm. Confining `unsafe` here lets every consumer crate
  keep `#![forbid(unsafe_code)]` while still publishing probes through
  the safe `fire_<N>` wrappers.
- `#[expect(clippy::items_after_statements, ...)]` on each `fire_<N>`
  body — the `usdt::provider!`-generated probe macro mixes per-call
  `static` items with `let _: u64 = ...` arg-bindings. Reordering to
  satisfy clippy is impossible: the shape is fixed by an external
  crate. The wrapper functions exist precisely to absorb this lint
  noise once, behind a stable API.

Anyone tempted to add `#[allow]`/`#[expect]` elsewhere in this crate
should first consider whether they can route through `fire_<N>`
instead. Workspace policy is to keep the suppressions inside this
file only.

## Macro arity ceiling

`usdt` v0.6 caps a single provider's probe at six wire arguments;
`fire_<N>` mirrors that ceiling. Probes wanting more than six fields
must either (a) decompose into multiple probe sites or (b) fold their
payload into a struct annotated with `#[derive(kithara_probes::Probe)]`,
which fans out fields through a single `fire_N` call.
