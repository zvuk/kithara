# kithara-xtask-core

`kithara-xtask-core` is the reusable, config-driven xtask command core being
extracted from kithara's existing `xtask` binary.

The extraction is in progress. The crate currently provides the workspace
context scaffold, and the reusable commands will move here over the next
refactoring waves.

Project-specific values are intended to come from `.config/xtask.toml`, with
documented code defaults where no project override is needed.

The crate is consumed by thin per-project `xtask` binaries that keep their own
project-specific commands while delegating shared tooling to this core.

Task 9 will expand this overview with a usage example and the crate-level
`CONTEXT.md`.
