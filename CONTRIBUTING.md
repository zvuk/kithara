# Contributing to kithara

Bug reports, feature requests, and pull requests are welcome. For a large
change, open an issue first so the design can be agreed before the code.

## Setup

System packages: `libasound2-dev` on Linux, Xcode Command Line Tools on macOS.

```bash
git clone https://github.com/zvuk/kithara.git && cd kithara
cargo install just --locked
just tooling install                       # nextest, ast-grep, typos, formatters, audits
cargo install cargo-hakari --locked
rustup toolchain install nightly --component rustfmt
```

Optional: `prek install -f` (`pip install prek`) runs `just lint fast` before
each commit.

## Everyday commands

| Command | What it does |
| --- | --- |
| `just test` | Product test suite (nextest, virtual clock) |
| `just test run seek` | Tests whose name contains `seek` |
| `just test run -E 'package(kithara-hls)'` | Any nextest filter expression |
| `just test all` | Suite plus doc-tests |
| `just fmt` | Format Rust, TOML, JSON, Markdown |
| `just check clippy` | Clippy over the workspace |
| `just lint fast` | Formatting, Clippy, ast-grep, typos, architecture and style rules |
| `cargo run -p kithara-app` | Desktop demo app |

Recipes live in `.config/just/`. [TESTING.md](TESTING.md) covers the test
harness and the virtual clock.

## Pull requests

Before opening a PR:

```bash
just fmt check
just lint fast
just test
```

- One change per PR, with a test that fails without it.
- Commit messages follow [Conventional Commits](https://www.conventionalcommits.org/):
  `fix(hls): ...`, `feat(play): ...`.
- New warnings and lint suppressions are not accepted; fix the code instead.

[AGENTS.md](AGENTS.md) holds the full code rules (shared by human and AI
contributors). Crate contracts are on the [wiki](https://github.com/zvuk/kithara/wiki/kithara).

## Mobile builds

**Android AAR.** Needs the NDK (`ANDROID_NDK_HOME`), `cargo-ndk`, and the
targets `aarch64-linux-android` and `x86_64-linux-android`.

```bash
just platform android aar
```

Output: `android/lib/build/outputs/aar/kithara.aar` and `kithara-okhttp.aar`
(HTTP over OkHttp). See [android/README.md](android/README.md).

**Apple XCFramework.** Needs Xcode and the targets `aarch64-apple-ios`,
`aarch64-apple-ios-sim`, `aarch64-apple-darwin`, `x86_64-apple-darwin`.

```bash
just platform apple xcframework                   # release
just platform apple xcframework --profile debug   # faster local builds
```

Output: `apple/KitharaFFIInternal.xcframework`. To build an app against the
local checkout, set `KITHARA_DIR=/path/to/kithara KITHARA_LOCAL_DEV=1`; see
[apple/README.md](apple/README.md).

## License

Contributions are licensed under [MIT](LICENSE-MIT) OR [Apache-2.0](LICENSE-APACHE).
