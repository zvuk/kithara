<div align="center">

<img src="https://raw.githubusercontent.com/zvuk/kithara/main/logo.svg" alt="kithara" width="300">

</div>

<div align="center">

[![crates.io](https://img.shields.io/crates/v/kithara-ui-shaping.svg)](https://crates.io/crates/kithara-ui-shaping)
[![docs.rs](https://docs.rs/kithara-ui-shaping/badge.svg)](https://docs.rs/kithara-ui-shaping)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](https://github.com/zvuk/kithara/blob/main/LICENSE-MIT)

</div>

# kithara-ui-shaping

Turns a string and a text style into positioned glyphs. The crate owns the
embedded font catalog (Inter, Space Grotesk, JetBrains Mono and Lucide), the
choice of face for a family and weight, the fallback faces for scripts the
display family does not carry, and whether the machine's own fonts may answer.
It knows no toolkit: a backend paints the `GlyphRun` it returns.

## Usage

```rust
use kithara_ui_shaping::{FontFamily, FontWeight, TextContext, TextStyle};

let mut text = TextContext::new()?;
let style = TextStyle {
    font: FontFamily::Sans,
    weight: FontWeight::Semibold,
    size: 12.0,
    spacing: 0.0,
};
let run = text.shape("GAIN", style, None);
assert!(run.width() > 0.0);
# Ok::<(), kithara_ui_shaping::TextError>(())
```

## Key Types

<table>

<tr><th>Type</th><th>Role</th></tr>

<tr><td><code>TextStyle</code> / <code>FontFamily</code> / <code>FontWeight</code></td><td>The style a document names for one run of text</td></tr>

<tr><td><code>FontId</code></td><td>One embedded face, its bytes, and the face that answers a family at a weight</td></tr>

<tr><td><code>FontPolicy</code> / <code>TextResources</code></td><td>Which collections may answer, and the registered faces and outlines a backend paints from</td></tr>

<tr><td><code>TextContext</code></td><td>Shapes and measures text, editable lines with their caret offsets, and Lucide icons</td></tr>

<tr><td><code>GlyphRun</code> / <code>GlyphSegment</code> / <code>GlyphFace</code></td><td>The shaped result: segments of positioned glyphs, each set in one resolved face</td></tr>

</table>

## Features

<table>

<tr><th>Feature</th><th>Default</th><th>Effect</th></tr>

<tr><td><code>shape</code></td><td>yes</td><td>The embedded faces and the Parley shaper; off, only the style vocabulary remains</td></tr>

</table>

## Integration

`kithara-ui` re-exports this crate as `kithara_ui::shaping` and its skin names
`FontFamily` and `FontWeight` from here. Its `render` feature turns `shape` on;
a build that parses documents without drawing them carries only the styles.

See [crate contracts](https://github.com/zvuk/kithara/wiki/kithara-ui-shaping) for detailed contracts, invariants, and internals.
