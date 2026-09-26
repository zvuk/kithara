<div align="center">

<img src="https://raw.githubusercontent.com/zvuk/kithara/main/logo.svg" alt="kithara" width="300">

</div>

<div align="center">

[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](https://github.com/zvuk/kithara/blob/main/LICENSE-MIT)

</div>

# kithara-test-fixtures

Audio test assets produced at build time and served from a persistent store
on disk. A test asks for bytes and gets them; nothing is synthesized or
encoded inside a test's wall-clock deadline.

Source edits, dependency updates and commits do not invalidate prepared assets.
The explicit `cache-version` file selects the shared cache revision. Change it
only when intentionally replacing the cached fixture set; use a new case name
for an individual replacement. Rebuilds reuse existing entries.

An asset stored in a format another crate owns names a sample of that format
with `#[kithara::asset(format = sample_fn)]`. The sample's digest joins the
case id, so an edit to the format re-addresses every such case and the next
build produces it again: no format version bump and no `cache-version` change.
Every `.analysis` asset is keyed this way by `analysis_format`, one analysis
file written with the current `kithara-analysis`.

With the `rhythm` family on, every whole audio track (`content_type = "audio/*"`) carries a build-time analysis beside it, `analysis_{accessor}`: the
production beat pass over the decoded track. Its key also carries the beat
pass's cache tag (`analysed_format`), so a new detector or grid re-analyses
every track instead of serving an older grid. Audio bytes that are no whole
track — a segment, an init header, data malformed on purpose — are declared
`fragment` and carry none; a track assembled from fragments declares its
analysis itself. Generated rhythm tracks keep their generator-truth analyses as
well, so a test can run on either grid and measure how far the analyser drifts.

`KITHARA_FIXTURE_REFRESH` overrides that reuse for one build: `all` rebuilds the
whole revision, and a comma-separated list rebuilds only what it names: an
accessor (`{func}_{case}`), or a producing function (`{func}`), which stands for
every case it registers. Every asset derived from a selected one is rebuilt with
it, so the cache never holds a dependent that disagrees with its source. A name no
enabled family registers is reported as a build warning, not an error: the asset
set follows the enabled families, so one selection is read by builds that
register different halves of it. Fetching families are never selected: without
hydration they cannot be produced again, so the store keeps what it holds.

Set `KITHARA_FIXTURE_CACHE` to an absolute persistent directory before building.
There is no temporary-directory default. For all local worktrees, configure it
once in your user Cargo configuration (`~/.cargo/config.toml`):

```toml
[env]
KITHARA_FIXTURE_CACHE = "/absolute/persistent/path/kithara-fixtures"
```

The environment can override this value. CI supplies a persistent directory
shared across branches and platforms within each trust boundary. When changing
the root, copy the existing version directory to preserve prepared assets.

Native test binaries also read `KITHARA_FIXTURE_CACHE` at first fixture access.
This selects one local root for both `Asset::bytes()` and `Asset::path()`. With
no runtime override, they use the root selected at build time. An explicit
override must be absolute; missing entries fail without consulting the build
store.

When `KITHARA_FIXTURE_ORIGIN` is also set to `http://127.0.0.1:<port>`, that
HTTP origin is the record source and `KITHARA_FIXTURE_CACHE` is only the local
replica. The store selects the source once from this configuration; it does
not try disk and then the network. ART points the origin at the host fixture
server through `adb reverse` and keeps the replica in the session directory so
a later process can reuse a fetch. Host and wasm lanes leave the origin unset.

To run a binary on another machine or device without an origin, stage the
version directory under the configured runtime root. Assets marked `embed`
also use this store on native targets.

`kithara-fixture-export --manifest output.json` records the selected root,
revision, and every namespace file with its root-relative path, SHA-256 and
byte length. It includes nested HLS resources, excludes producer locks and
temporary writes, and rejects symlinks or paths that escape the store. The
existing `kithara-fixture-export <accessor-name> <output-path>` exports one asset.

## Usage

```rust
use kithara_test_fixtures::fixtures::tone_mp3;

#[kithara::test]
fn decode_prepared_audio(tone_mp3: &'static [u8]) {
    // Pass the prepared bytes to the decoder under test.
    assert!(!tone_mp3.is_empty());
}
```

Fixture providers read already-built assets. Signal generation, including tiny
PCM inputs, belongs in `src/defs/` and runs through `build.rs`. Async providers
may own local servers and return them with the prepared input; test parameters
use `#[future(awt)]` to receive those resources after preparation.

## Key Types

- `store::STORE_ENV` — `KITHARA_FIXTURE_CACHE`, the required store root. CI
  points it at a persisted directory so a fresh job starts warm.
- `store::ORIGIN_ENV` — `KITHARA_FIXTURE_ORIGIN`, optional `http://127.0.0.1`
  source. When set, records come from this URL and land in `STORE_ENV`.
- `store::asset_id` — stable identity of one case.
- `store::formatted_asset_id` — identity of one case keyed by its format sample.
- `store::file` — local path of one store-relative record, fetched when an
  origin is configured.
- `store::read_entry` / `store::write_entry` — a hit-or-miss read and an atomic
  write; an empty file counts as a miss.
- `store::lock_entry` — the exclusive producer lock for one entry.
- `signal::Wave` — the waveform vocabulary.
- `signal::Pcm` — interleaved 16-bit PCM in memory.
- `signal::wav` / `signal::header` — the RIFF writer.

### Layout

- `src/defs/` — generator bodies, one function per asset, each carrying its
  cases. These compile into the build script only, never into the library.
- `src/signal/` — waveforms, PCM buffers, and the RIFF writer. The workspace's
  one waveform implementation for build-time inputs and signal assertions.
- `src/fmp4/` — the fMP4 mux: an `EncodedTrack` in, init and media segments out.
  The build script packages both embedded bodies and registered HLS variants.
- `build.rs` — resolves every declared case against the store, produces what is
  missing, and writes the accessor module.
- `src/store/` — identity, namespace, atomic writes, the producer lock, and the
  optional HTTP origin that ART uses as the one record source.

An asset declared `#[kithara::asset(..., embed)]` is baked into wasm binaries
with `include_bytes!`, because wasm has no fixture filesystem. Native targets
read the same asset from the store at run time. It is generated once, into the
store, like every other asset.

See [crate contracts](https://github.com/zvuk/kithara/wiki/kithara-test-fixtures)
for the store layout, invalidation, and build-time preparation contracts.
