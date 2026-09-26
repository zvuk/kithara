<div align="center">

<img src="https://raw.githubusercontent.com/zvuk/kithara/main/logo.svg" alt="kithara" width="300">

</div>

<div align="center">

[![crates.io](https://img.shields.io/crates/v/kithara-ui-capture.svg)](https://crates.io/crates/kithara-ui-capture)
[![docs.rs](https://docs.rs/kithara-ui-capture/badge.svg)](https://docs.rs/kithara-ui-capture)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](https://github.com/zvuk/kithara/blob/main/LICENSE-MIT)

</div>

# kithara-ui-capture

Photographs sets of UI pages into PNG files and compares two sets pixel by
pixel. A host implements `Stage` — open a page, advance its clock, rasterise it
— and the crate owns everything else: the walk over a film of pages, the file
names and recorded geometry of a set, cutting one control out of a photograph,
and the comparison of two sets against a per-page budget. It knows no toolkit.

## Usage

```rust
use kithara_ui_capture::{Film, Geometry, Stage, shoot_set};

struct Blank {
    pixels: Vec<u8>,
}

impl Stage for Blank {
    type Page = &'static str;

    fn geometry(&self) -> Geometry {
        Geometry { height: 1, scale: 1.0, width: 1 }
    }

    fn shoot(&mut self) -> Result<&[u8], String> {
        Ok(&self.pixels)
    }

    fn tick(&mut self) {}

    fn turn(&mut self, _page: &Self::Page) -> Result<(), String> {
        Ok(())
    }
}

let dir = std::env::temp_dir().join("kithara-ui-capture-readme");
let mut stage = Blank { pixels: vec![0, 0, 0, 255] };
let written = shoot_set(&mut stage, &Film::stills(vec!["home"]), &dir)?;
assert_eq!(written, [dir.join("home.png")]);
# Ok::<(), String>(())
```

## Key Types

<table>

<tr><th>Type</th><th>Role</th></tr>

<tr><td><code>Stage</code></td><td>One host, turned to a page at a time and rasterised</td></tr>

<tr><td><code>Film</code> / <code>shoot_set</code></td><td>Which pages a set photographs, how often, and the walk that writes them</td></tr>

<tr><td><code>Locate</code> / <code>shoot_part</code></td><td>Cuts the region one control was laid out over out of a photograph</td></tr>

<tr><td><code>diff::compare</code></td><td>Compares two sets page by page and judges them against a budget</td></tr>

</table>

## Integration

`kithara-ui` depends on this crate under its `capture` feature and adds the
stages that rasterise its two hosts offscreen; the gallery example and the UI
parity tests drive both through it.

See [crate contracts](https://github.com/zvuk/kithara/wiki/kithara-ui-capture) for detailed contracts, invariants, and internals.
