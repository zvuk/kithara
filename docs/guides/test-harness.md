# Test Harness

Use this when adding or debugging tests, changing test utilities, or explaining
validation scope.

## Acceptance

- `cargo xtask test` and `just test` are the acceptance entrypoints.
- Raw `cargo test` or `cargo nextest` is a scoped probe, not a final claim.
- If a probe is reported, name the package, filter, lane, and why it is enough
  for that local question.

### Axes

- `flash` is the default axis and defaults ON.
- `no-block` is off by default; enable with `--no-block=on` for poll-blocking
  detector coverage.
- `just gate` keeps two explicit lanes: flash ON + no-block ON, and flash OFF.
- Tests that verify detector behavior are gated behind the `no-block` feature.

## Regression Tests

- A regression test must fail on the broken surface and pass after the fix.
- Prove the right reason: setup preconditions must be asserted, and the fixed
  code path must be load-bearing.
- Flash-sensitive changes should verify the relevant runtime surface. If
  `flash=off` is required because the test is real-time or live I/O, say so.
- Loom models run through `just test --loom=on`; add `--flash=on` only when the
  modeled contract also requires Flash virtual-time behavior.

## Harness Shape

- Use shared helpers from `kithara-test-utils` and `tests/src` for temp dirs,
  servers, waits, fixtures, flash pacing, and spawned work.
- Do not hard-code ports or random global paths.
- Wait for observable conditions, events, or bounded predicates. Do not sleep
  arbitrary wall-clock windows unless the test is explicitly real-time.
- Live-network, device, or OS-surface tests must be marked by lane/attributes and
  must not be treated as deterministic unit coverage.

## Organization

- Add coverage to the owner suite or existing module test first.
- Create a new regression file only for a real named reproduction that does not
  fit an owner suite.
- Tests should assert contracts: state, events, bytes, positions, typed errors,
  or resource cleanup. Do not test only that nothing panicked.
