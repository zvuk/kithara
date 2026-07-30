set shell := ["bash", "-euo", "pipefail", "-c"]
set working-directory := "../.."

modules CRATE:
    cargo modules structure --package {{ CRATE }}

[positional-arguments]
orphans *ARGS:
    just _xtask orphans "$@"

# Render the workspace dependency graph as target/depgraph.svg.
graph:
    @command -v dot >/dev/null || { echo "graphviz 'dot' is required (brew install graphviz)"; exit 2; }
    @mkdir -p target
    cargo depgraph --workspace-only --dedup-transitive-deps | dot -Tsvg > target/depgraph.svg
    @echo "==> target/depgraph.svg"

# Build the revision-scoped Mermaid architecture document.
[positional-arguments]
viz *ARGS:
    just _xtask viz "$@"
