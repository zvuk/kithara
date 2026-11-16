## Description

Brief description of the changes.

## Draft Tests

<!--
A draft runs the required test packages (`[test.required]` in
`.config/xtask.toml`) plus what this block names. A ready pull request runs
everything and ignores it. Both keys are optional:
  packages = ["kithara-file-tests"]
  filter = "test(/seek_past_end/)"
-->

```kithara-tests
```

## Type of Change

- [ ] Bug fix
- [ ] New feature
- [ ] Refactoring (no functional changes)
- [ ] Documentation
- [ ] Other (describe):

## Checklist

- [ ] `cargo fmt --all --check` passes
- [ ] `just check clippy` passes
- [ ] Tests added/updated
- [ ] No `unwrap()`/`expect()` in production code
- [ ] Documentation updated (if applicable)
