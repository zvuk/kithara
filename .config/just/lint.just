set shell := ["bash", "-euo", "pipefail", "-c"]
set working-directory := "../.."

# Run all workspace architecture, style, and idiom ratchets.
[default]
[positional-arguments]
all *ARGS:
    just _xtask lint "$@"

[positional-arguments]
arch *ARGS:
    just _xtask lint arch "$@"

[positional-arguments]
style *ARGS:
    just _xtask lint style "$@"

[positional-arguments]
idioms *ARGS:
    just _xtask lint idioms "$@"

_shared:
    just fmt check
    just _xtask config project --check
    just check clippy
    just lint ast-grep
    just lint typos
    just lint arch
    just lint style

# Fast local chain: formatting, Clippy, ast-grep, typos, and both ratchets.
fast:
    just lint _shared
    just lint idioms --check derivable_getter --check derivable_delegation

# Merge gate chain: the fast chain plus this tool's own tests. They resolve the
# shipped package graph and the configured lane commands as they stand, so they
# answer a diff and belong before a merge rather than after it. They stay out of
# `fast`, which the commit hook runs: a git hook exports `GIT_DIR`, and the tests
# that drive temporary repositories inherit it.
gate:
    just lint fast
    cargo test -p xtask

# Full CI chain: fast checks, xtask tests, quality scans, and idioms.
# It runs every idiom check below, so it takes the shared part directly rather
# than through `fast`, whose two-check pass it would then repeat.
full:
    just lint _shared
    cargo test -p xtask
    just quality unimock-check
    just quality rstest-audit
    just quality trait-mock-audit
    just quality trait-mock-exceptions
    just lint idioms

[positional-arguments]
ast-grep *ARGS:
    just _xtask ast-grep "$@"

[positional-arguments]
audit-clippy *ARGS:
    just _xtask audit-clippy "$@"

[positional-arguments]
typos *ARGS:
    just _xtask typos "$@"

[positional-arguments]
similarity *ARGS:
    just _xtask similarity "$@"

[positional-arguments]
deadlock *ARGS:
    exec cargo "+${KITHARA_LOCKBUD_TOOLCHAIN:-nightly}" lockbud -k deadlock --workspace "$@"

unused-pub:
    cargo workspace-unused-pub
