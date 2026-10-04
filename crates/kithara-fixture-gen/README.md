<div align="center">

<img src="https://raw.githubusercontent.com/zvuk/kithara/main/logo.svg" alt="kithara" width="300">

</div>

<div align="center">

[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](https://github.com/zvuk/kithara/blob/main/LICENSE-MIT)

</div>

# kithara-fixture-gen

Workspace crate (`publish = false`) that generates the audio test assets. Every
`#[kithara::asset]` definition here registers itself at link time; `generate`
orders the definitions by what they are built from, materializes each case into
the shared store, and writes the accessors `kithara-test-fixtures` compiles.

It is only ever a build dependency, which keeps the encoders, the analysers and
the HTTP client out of every target build.

A build reuses every entry the store already holds.
`KITHARA_FIXTURE_REFRESH` overrides that reuse for one build: `all` rebuilds the
whole revision, and a comma-separated list rebuilds only what it names: an
accessor (`{func}_{case}`), or a producing function (`{func}`), which stands for
every case it registers. Every asset derived from a selected one is rebuilt with
it, so the cache never holds a dependent that disagrees with its source. A name no
enabled family registers is reported as a build warning, not an error: the asset
set follows the enabled families, so one selection is read by builds that
register different halves of it. Fetching families are never selected: without
hydration they cannot be produced again, so the store keeps what it holds.

## Usage

```rust,ignore
// build.rs of kithara-test-fixtures
fn main() {
    kithara_fixture_gen::generate();
}
```

## Key Types

<table>

<tr><th>Type</th><th>Role</th></tr>

<tr><td><code>generate</code></td><td>Resolves every registered case against the store, produces what is missing, and writes the accessor module into <code>OUT_DIR</code></td></tr>

</table>

## Features

Each family registers the assets declared for it; a build with none registers
only the synthetic PCM they all build on.

- `wav`, `encoded`, `signal` — synthetic PCM and its encoded forms.
- `packaged`, `hls`, `hls-inputs` — fMP4 packaging and the HLS variants built
  from it.
- `rhythm` — generated rhythm tracks and the beat analysis of every whole track.
- `remote`, `library` — assets fetched at build time and verified by digest.

## Integration

`kithara-test-fixtures` names this crate in `[build-dependencies]` only and
forwards each of its families to the family of the same name here.

Signal fixtures use `kithara-encode::EncoderFactory` to encode synthetic PCM.
APE encoding uses the Monkey's Audio SDK through the `monkeys-audio` feature;
the other formats use FFmpeg bindings. The SDK version is pinned in
`.config/ci-pins.toml` and provisioned by CI. On macOS, install the `mac`
Homebrew formula; elsewhere, set `MONKEYS_AUDIO_DIR` to the SDK install prefix
when it is outside system library paths. No encoded APE is checked in.
