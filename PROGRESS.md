# Progress

What is in flight right now. The
[GitHub Projects board](https://github.com/users/gerasim13/projects/3) owns
capability status and the roadmap, and git owns the facts. This file owns
intent: what is being worked on, what comes next, what is stuck. Update it in
the change that lands the work, and keep it short.

## In Flight

- Build and test warnings, cleared. `Atomic*::fetch_update` is deprecated as of
  1.95, and its replacement cannot be spoken here: `loom` 0.7.2 carries only the
  old name, so anything routed through `kithara_platform::sync::atomic` would
  break the loom lane. The four sites moved to the `compare_exchange_weak` loop
  `fetch_update` is documented to compile into, which both toolchains have and
  which keeps every ordering. MSRV is 1.95, `rkyv` 0.8.18 and `bytecheck` 0.8.3
  retire their own deprecations, and `kithara-app`'s GUI-only modules are gated
  on `gui` so a `lib-only` build stops warning about 57 unused items.
- The `sccache` trap in the Clippy path, closed. A workstation Clippy run set
  `CARGO_INCREMENTAL=1` to cancel the blanket `0` the `justfile` exports, but
  `sccache` reads that variable too and aborts rather than fall back, for any
  language: `btls-sys` reached its own C compiler through a CMake launcher that
  refused to run, and printed no compiler error because no compiler ran.
  Clearing both variables says the same thing to Cargo - workspace crates are
  incremental by default and registry ones never - and nothing to anyone else.
  No site in the repository sets a non-zero `CARGO_INCREMENTAL` now.

## Next

- `[profile.release] opt-level = "z"` builds every native and DSP dependency for
  size, so `signalsmith-stretch`, `rubato` and `symphonia-bundle-mp3` are less
  optimised in a release build than in a debug one, where explicit
  `[profile.dev.package.*]` entries give them 2 or 3. The suite is unaffected:
  17 of 18 lanes build `test-release`, which is `opt-level = 3` and reaches the
  C and C++ dependencies through `OPT_LEVEL`. This is a shipping decision, not a
  test one, and it wants its own change.
- `block` 0.1.6 is a future-incompat report no change here can answer: it
  reaches the tree through `cpal` and has no published successor.
- `kithara-ui` still warns under `--no-default-features --features render` and
  `--features vello`, where the widget layer compiles without a host. That is
  627 items and its own change.
- Work the comment queue down by hand. `--fix` is exhausted for comments - a
  second run on a clean tree changes nothing - so all 668 are decisions: 497
  comments carrying prose outside a doc comment, 105 doc blocks past a dozen
  lines, 50 oversized inline comments, 16 dense functions. A body comment has no
  mechanical destination.
- 439 ordering findings are still mechanical: `struct_field_order` 160,
  `trait_item_order` 188, `struct_init_order` 91. One `just lint style --fix`
  clears them, but it rewrites declarations across every crate, so it wants its
  own change.
- Wire `just lint style` to a gate. Nothing runs it today - not the commit hook,
  not a CI lane - which is why the ratchet drifted unseen. A warm run is 58 s:
  too much for every commit, nothing for a lane. The lane catalog owns that
  change, so it does not belong in this one.

## Blocked

- Nothing.
