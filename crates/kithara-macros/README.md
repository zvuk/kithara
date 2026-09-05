<div align="center">

<img src="https://raw.githubusercontent.com/zvuk/kithara/main/logo.svg" alt="kithara" width="300">

</div>

<div align="center">

[![crates.io](https://img.shields.io/crates/v/kithara-macros.svg)](https://crates.io/crates/kithara-macros)
[![docs.rs](https://docs.rs/kithara-macros/badge.svg)](https://docs.rs/kithara-macros)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](https://github.com/zvuk/kithara/blob/main/LICENSE-MIT)

</div>

# kithara-macros

Proc-macro crate for Kithara's production code. It provides `#[derive(Patch)]`:
from one configuration struct it generates `<Struct>Patch`, the shape a
configuration document may say about it, and the `apply` that merges one onto
the other. A crate keeps a single configuration struct; the patch beside it is
generated, never written.

## Usage

```rust
use kithara_macros::Patch;

#[derive(Patch)]
pub struct HlsConfig<S> {
    /// The caller hands this over; a document cannot name it.
    #[patch(skip)]
    pub store: AssetStore<S>,
    /// Max segments to download per step.
    pub download_batch_size: usize,
    /// Max bytes the downloader may run ahead of the reader.
    pub look_ahead_bytes: Option<u64>,
}

let patch: HlsConfigPatch = serde_yaml_ng::from_str("download_batch_size: 5\n")?;
config.apply(patch);
```

## Key Types

Derive macros:

- `#[derive(Patch)]` — generates `<Struct>Patch` and `<Struct>::apply`

Field attributes:

- `#[patch(skip)]` — the field is not a document key. Naming it is refused by
  name rather than dropped silently.
- `#[patch(nested)]` — the field's own type has a patch; the document names it
  under a key of the same name and the merge recurses.
- `#[patch(attribute(...))]` — one attribute added to the generated patch field
  alone, for example `serde(with = "humantime_serde::option")` on a `Duration`.

## Integration

Used by every crate that owns a configuration a document may reach, and by
`kithara-app`, which deserializes the generated patches out of `app.yaml` and
applies them onto the configurations it built.

See [CONTEXT.md](CONTEXT.md) for the contract the generated code keeps.
