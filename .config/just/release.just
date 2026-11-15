set shell := ["bash", "-euo", "pipefail", "-c"]
set working-directory := "../.."

# Publish public crates to crates.io in dependency order.
[positional-arguments]
crates *ARGS:
    just _xtask publish "$@"

# Build and sign the universal macOS application and its release archive.
[positional-arguments]
app *ARGS:
    just _xtask release app "$@"

# Render CHANGELOG.md from the history and the release tags; a VERSION names
# the unreleased commits as that release.
[positional-arguments]
changelog *VERSION:
    just _xtask release changelog ${1:+--version "$1"}

# Lay the Pages site out from a directory of release artifacts, to look at
# before a release publishes it.
[positional-arguments]
site VERSION ARTIFACTS OUTPUT:
    just _xtask release site --version "$1" --artifacts "$2" --output "$3"
