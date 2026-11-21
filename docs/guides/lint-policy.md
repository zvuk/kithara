# Lint Policy

Open this only when touching lint policy, lint exceptions, or a failing lint that
cannot be resolved locally.

## Zero Suppressions

Forbidden in production code:

- new entries in `.config/{arch,style,idioms,ast-grep}/*.toml`;
- `#[allow(...)]`;
- `#[expect(...)]`;
- `#![allow(...)]`;
- `// xtask-lint-ignore:`;
- `#[rustfmt::skip]`;
- baseline growth.

An unavoidable lint means STOP and fix the underlying code. Do not make a commit
pass by suppressing the symptom.

Cast-group fixes use `num-traits` (`NumCast`, `AsPrimitive`, `ToPrimitive`), not
`as` or per-site `try_from().expect()`.

## Sanctioned Existing Exceptions

There are exactly two flash-ON-scoped expects in `kithara-platform/src/lib.rs`:

- `#[cfg_attr(feature = "flash", expect(unreachable_pub, dead_code, unused_imports))]`
  on `mod native`;
- `#[cfg_attr(all(not(target_arch = "wasm32"), feature = "flash"), expect(unreachable_pub, dead_code))]`
  on `mod common`.

They cover known flash-ON facade false positives and inert control surfaces. The
OFF lane keeps full lint coverage. Unfulfilled expectations should be removed
when the corresponding flash wrappers consume the code.

`smoothing_primitive_sites.exempt_crates` in `.config/arch/thresholds.toml`
lists test and tooling crates plus exactly one production crate,
`kithara-abr`.

- Why: the crate owns network throughput statistics. Its bandwidth estimator is
  an `f64` EWMA with a sample-dependent weight and bias correction, which no
  structural rule can tell apart from audio-parameter smoothing. Rewriting it
  onto the audio smoothing primitive would change the algorithm.
- Scope: the mechanism skips the whole crate, including code added later, not
  only the estimator. The check therefore no longer catches hand-rolled
  audio-parameter smoothing inside `kithara-abr`; adding any audio-parameter
  processing to that crate requires revisiting this exception first.
- Unchanged: every other crate, including any new one, stays checked by
  default. The same EWMA shape outside `kithara-abr` is still reported.
- This exception authorizes no other configuration entry, allowed file,
  baseline change, or source suppression.

## Shared Configuration

- Workspace config owns policy: `rustfmt.toml`, `clippy.toml`, `deny.toml`,
  `.config/tomlfmt.toml`, `.config/typos.toml`, and `sgconfig.yml`. Change lint
  policy there instead of creating per-crate drift.
- Treat rustc warnings (`unused-imports`, `dead_code`, `deprecated`, unfulfilled
  `#[expect]`, `unreachable_pub`) exactly like Clippy findings: fix the cause in
  the code you touch rather than letting warnings accumulate.
- The pre-commit hook expects clean formatting, linting, and tests. If they fail
  after your change, assume your change caused it until proven otherwise.

## Failing Lints

- Treat rustc, Clippy, ast-grep, xtask, formatter, and dependency-audit findings
  as design feedback.
- Open only the reported rule/check by id.
- Fix ownership, API, layer, or Rust-shape cause.
- Do not scan the full lint tree before every task.
