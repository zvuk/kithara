set shell := ["bash", "-euo", "pipefail", "-c"]
set working-directory := "../.."

# Execute one typed CI lane. GitLab jobs should only route through this recipe.
[positional-arguments]
run *ARGS:
    just _xtask ci run "$@"

# Render the jobs a role schedules for one pipeline kind, as JSON.
[positional-arguments]
lanes *ARGS:
    just _xtask ci lanes "$@"

# Check the draft's pull request test block in `KITHARA_PR_TESTS`.
[positional-arguments]
pr-tests *ARGS:
    just _xtask ci pr-tests "$@"

# Run one declared lane in an environment the executor already prepared.
[positional-arguments]
lane *ARGS:
    just _xtask ci lane "$@"

# Provision or maintain a CI machine: its images, runners and agents.
[positional-arguments]
host *ARGS:
    just _xtask ci host "$@"

# Build a pinned CI container image for this machine's architecture.
[positional-arguments]
image *ARGS:
    just _xtask ci image "$@"

# Reconcile the private GitLab repository with the public GitHub repository.
[positional-arguments]
bridge *ARGS:
    just _xtask ci bridge "$@"

# Run every lane of a repeated-test evidence run.
[positional-arguments]
stress *ARGS:
    just _xtask stress run "$@"

# Independently verify and render a downloaded stress artifact.
[positional-arguments]
stress-report *ARGS:
    just _xtask stress report "$@"

# Run all linters scoped to a crate, path, or workspace.
[positional-arguments]
audit *ARGS:
    just _xtask audit "$@"

# Dual-run gate: flash ON with no-block, then flash OFF.
[positional-arguments]
gate *ARGS:
    just _xtask ci gate "$@"

# Comprehensive workspace health report.
health:
    just _xtask health

# Consolidate one run's quality artifacts into a single markdown report.
[positional-arguments]
report *ARGS:
    just _xtask ci-report "$@"

[positional-arguments]
windows-diagnostics *ARGS:
    just _xtask ci windows-diagnostics "$@"

# Full CI cycle with independent stage logs.
[positional-arguments]
full OUTPUT:
    just _xtask ci full "$@"

# Operate the host-local shared compiler cache with reviewed pins.
[positional-arguments]
cache ENV_FILE *ARGS:
    just _xtask ci cache compose "$@"

# Prove reuse between two independent compiler-cache daemons.
[positional-arguments]
cache-verify *ARGS:
    just _xtask ci cache verify "$@"

# Compile outside a declared lane with the same required RustFS policy.
[positional-arguments]
cache-exec PHASE +ARGS:
    just _xtask ci cache exec "$@"
