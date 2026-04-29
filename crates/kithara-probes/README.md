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
