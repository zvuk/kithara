# Contributing to kithara

Bug reports, feature requests, and code contributions are welcome.

## Quick Start

```bash
git clone https://github.com/<your-fork>/kithara.git
cd kithara
cargo install just --locked
cargo build --workspace
just test-all          # nextest + doctests
just lint-fast         # fast policy checks
just lint-full         # full lint / policy checks
```

Complete the environment setup below before relying on the `just` workflow. The
canonical local workflow for both humans and AI agents lives in
[.docs/workflow/rust-ai.md](.docs/workflow/rust-ai.md).

## Environment Setup

### Host dependencies

- Linux: install `libasound2-dev`
- macOS: install Xcode Command Line Tools

### Required tooling

- `cargo-nextest` for `just test*`
- `ast-grep` for `just lint-fast` and `just lint-full`
- nightly `rustfmt` for `just fmt` and `just fmt-check`
- `prek` for the configured pre-commit and pre-push hooks

```bash
cargo install cargo-nextest --locked
cargo install ast-grep --locked
rustup toolchain install nightly --component rustfmt
```

Install `prek` with your preferred Python toolchain manager, for example:

```bash
python3 -m pip install --user prek
prek install -f
```

### Recommended tooling

- `cargo-deny` (dependency audits), `cargo-machete` (unused deps),
  `cargo-hack` (feature-powerset), `cargo-semver-checks` (public API compat)

```bash
cargo install cargo-deny cargo-machete cargo-hack cargo-semver-checks --locked
```

### Optional tooling

- `worktrunk` — convenience wrapper over `git worktree`
- `wasm-slim` — wasm size checks
- `critcmp` — benchmark comparison
- `chromedriver` (or another WebDriver binary) — browser-based flows

## Building Mobile Libraries

Two verified mobile build flows ship in the repo.

### Android AARs

Prerequisites: Android NDK installed with `ANDROID_NDK_HOME` exported,
`cargo-ndk` (`cargo install cargo-ndk`), and the Android targets:

```bash
rustup target add aarch64-linux-android x86_64-linux-android
just android aar
```

This builds the Rust JNI libraries in release, generates Kotlin UniFFI
bindings, packages the Android library, and exports both release AARs:

- `android/lib/build/outputs/aar/kithara.aar` — the main library
- `android/lib/build/outputs/aar/rust-tls.aar` — must be distributed alongside it

Keep both files in the same local artifacts directory when consuming the AARs
directly, and add any remaining app-level dependencies your integration needs.

### Apple XCFramework

Prerequisites: Xcode + Command Line Tools, `cargo-swift`
(`cargo install cargo-swift`), and the Apple targets:

```bash
rustup target add aarch64-apple-ios aarch64-apple-ios-sim aarch64-apple-darwin x86_64-apple-darwin
just apple xcframework                 # release
just apple xcframework --profile debug # faster local iteration
```

Output: `apple/KitharaFFIInternal.xcframework`, with slices
`macos-arm64_x86_64`, `ios-arm64`, and `ios-arm64_x86_64-simulator` (the
simulator slice keeps that name on Apple Silicon — expected).

The verified local-development path is to build the XCFramework first, then
point the consumer Swift package or Xcode project at this repo via
`KITHARA_DIR` + `KITHARA_LOCAL_DEV=1`:

```bash
KITHARA_DIR=/absolute/path/to/kithara KITHARA_LOCAL_DEV=1 swift build

KITHARA_DIR=/absolute/path/to/kithara KITHARA_LOCAL_DEV=1 \
xcodebuild -project /absolute/path/to/App.xcodeproj \
  -scheme MyApp -destination "generic/platform=iOS Simulator" build
```

## Before Submitting a PR

```bash
just lint-full
just test-all
```

Run extra checks when the change affects multiple crates, public APIs, wasm,
perf-sensitive code, or workflow tooling. Use the validation scope from your
task packet or plan together with the touched crate `README.md` files to choose
the right local commands.

See [`AGENTS.md`](AGENTS.md) for the full coding rules and
[.docs/plans/_template.md](.docs/plans/_template.md) for the required task-plan
format on non-trivial changes.

## Architecture

See [ARCHITECTURE.md](ARCHITECTURE.md) for the workspace layout and dependency
graph. Each crate has its own `README.md`.

## License

Contributions are licensed under [MIT](LICENSE-MIT) OR [Apache-2.0](LICENSE-APACHE).
