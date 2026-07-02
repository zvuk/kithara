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

## Failing Lints

- Treat rustc, Clippy, ast-grep, xtask, formatter, and dependency-audit findings
  as design feedback.
- Open only the reported rule/check by id.
- Fix ownership, API, layer, or Rust-shape cause.
- Do not scan the full lint tree before every task.
