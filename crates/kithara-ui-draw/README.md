<div align="center">

<img src="https://raw.githubusercontent.com/zvuk/kithara/main/logo.svg" alt="kithara" width="300">

</div>

<div align="center">

[![crates.io](https://img.shields.io/crates/v/kithara-ui-draw.svg)](https://crates.io/crates/kithara-ui-draw)
[![docs.rs](https://docs.rs/kithara-ui-draw/badge.svg)](https://docs.rs/kithara-ui-draw)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](https://github.com/zvuk/kithara/blob/main/LICENSE-MIT)

</div>

# kithara-ui-draw

A toolkit-neutral picture: points and affine transforms, and a draw list of
fills, strokes, glyph runs and images that any backend can replay. The crate
owns the list's pooled buffers and their byte budget, the capabilities a
backend declares, where ink lands after a transform, and reading an SVG into an
outline. It knows no toolkit; iced conversions sit behind a feature.

## Usage

```rust
use kithara_ui_draw::{DrawListBuilder, Rect, Rgba};

let mut builder = DrawListBuilder::default();
builder.fill_rect(
    Rect { x: 0.0, y: 0.0, w: 10.0, h: 10.0 },
    Rgba { r: 1.0, g: 0.0, b: 0.0, a: 1.0 },
);
let list = builder.finish();
assert_eq!(list.commands().len(), 1);
```

## Key Types

<table>

<tr><th>Type</th><th>Role</th></tr>

<tr><td><code>Pt</code> / <code>Transform</code></td><td>Geometry a document's poses fold into, shared by expansion and drawing</td></tr>

<tr><td><code>DrawList</code> / <code>DrawListBuilder</code> / <code>DrawCmd</code></td><td>The recorded picture and the builder that fills it</td></tr>

<tr><td><code>DrawBuffers</code> / <code>DrawPoolLimits</code></td><td>The pooled buffers a list is built in, and the limits a document names for them</td></tr>

<tr><td><code>Backend</code> / <code>Caps</code> / <code>replay</code></td><td>The contract a renderer implements and the capabilities it declares</td></tr>

<tr><td><code>Path</code> / <code>Outline</code> / <code>outline</code></td><td>Vector outlines, and reading one from an SVG document</td></tr>

</table>

## Features

<table>

<tr><th>Feature</th><th>Default</th><th>Effect</th></tr>

<tr><td><code>list</code></td><td>yes</td><td>The draw list, its pools, SVG outlines and the backend contract; off, only geometry and pool limits remain</td></tr>

<tr><td><code>iced</code></td><td>no</td><td>Conversions to and from iced's colour, point and rectangle</td></tr>

</table>

## Integration

`kithara-ui` re-exports this crate as `kithara_ui::draw` and its geometry as
`kithara_ui::geom`. Its `render` feature turns `list` on and its `iced`
feature turns `iced` on; a build that parses documents without drawing them
carries only the geometry and the pool limits.

See [crate contracts](https://github.com/zvuk/kithara/wiki/kithara-ui-draw) for detailed contracts, invariants, and internals.
