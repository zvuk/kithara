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

# Print whether the branch changes any scoped test lane's owned paths.
[positional-arguments]
touched *ARGS:
    just _xtask ci touched "$@"

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
    #!/usr/bin/env bash
    set -eo pipefail
    echo "=== gate: flash ON + no-block ON ==="
    just test run --flash=on --no-block=on "$@"
    echo "=== gate: flash OFF ==="
    CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-target}/flash-off" just test run --flash=off "$@"

# Comprehensive workspace health report.
health:
    just _xtask health

# Consolidate one run's quality artifacts into a single markdown report.
[positional-arguments]
report *ARGS:
    just _xtask ci-report "$@"

# Full CI cycle with independent stage logs.
full OUTPUT:
    #!/usr/bin/env bash
    set -uo pipefail
    RUN_ID=$(date -u +%Y%m%dT%H%M%SZ)
    mkdir -p "{{ OUTPUT }}"
    echo "$RUN_ID" > "{{ OUTPUT }}/run-id"
    echo "=== ci full $RUN_ID starting at $(date -u -Iseconds) ==="

    run_stage() {
        local name=$1; shift
        echo ">>> stage $name started at $(date -u -Iseconds)"
        "$@" > "{{ OUTPUT }}/stage-$name.log" 2>&1
        local rc=$?
        echo "$rc" > "{{ OUTPUT }}/stage-$name.exit"
        echo ">>> stage $name finished with exit=$rc at $(date -u -Iseconds)"
    }

    run_stage lint    just lint fast
    run_stage test    just test
    run_stage mutants just test mutants run-all --output "{{ OUTPUT }}/mutants"

    echo "=== ci full $RUN_ID done at $(date -u -Iseconds) ==="

# Operate the host-local shared compiler cache with reviewed pins.
[positional-arguments]
cache ENV_FILE *ARGS:
    just _xtask ci cache compose "$@"

# Prove reuse between two independent compiler-cache daemons.
[positional-arguments]
cache-verify *ARGS:
    just _xtask ci cache verify "$@"
