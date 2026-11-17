<div align="center">

<img src="https://raw.githubusercontent.com/zvuk/kithara/main/logo.svg" alt="kithara" width="300">

</div>

<div align="center">

[![crates.io](https://img.shields.io/crates/v/kithara-ui-lottie.svg)](https://crates.io/crates/kithara-ui-lottie)
[![docs.rs](https://docs.rs/kithara-ui-lottie/badge.svg)](https://docs.rs/kithara-ui-lottie)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](https://github.com/zvuk/kithara/blob/main/LICENSE-MIT)

</div>

# kithara-ui-lottie

Prepared built-in and caller-owned Lottie artwork, and the emitter that draws
one supported frame into the toolkit-neutral draw list. A frame is drawn whole or refused whole: an artwork that asks for
something the list has no word for — a trim, a mask, a matte, a ramped stroke
— is refused by name and leaves the list untouched.

## Usage

```rust
use kithara_ui_draw::DrawListBuilder;
use kithara_ui_lottie::{builtin_artwork, emit};

let artwork = builtin_artwork("pulse").expect("the toolkit ships pulse");
let mut list = DrawListBuilder::default();
emit(artwork.composition(), artwork.frame_at(0.5, 2.0), &mut list)?;
assert!(!list.finish().commands().is_empty());
# Ok::<(), kithara_ui_lottie::LottieError>(())
```

## Key Types

<table>

<tr><th>Type</th><th>Role</th></tr>

<tr><td><code>Artwork</code> / <code>builtin_artwork</code></td><td>A shipped artwork by name, its authored box, and which frame stands at a point of a looping pass</td></tr>

<tr><td><code>emit</code></td><td>Draws one frame of a composition into a draw list</td></tr>

<tr><td><code>LottieError</code></td><td>What an artwork asks for that the draw list has no word for</td></tr>

</table>

## Integration

Call `Artwork::parse(bytes)` once when preparing caller resources. It returns
`ArtworkError` for malformed JSON, unsupported authored features, invalid values
or exceeded bounds. It checks the whole input, including layers inactive at the
initial, middle and final frames, before Velato can discard unsupported data.
Prepared compositions are immutable; `emit` performs no source IO or JSON parsing.

Admission is limited to 128 KiB, 64 levels of JSON nesting and 16,384 JSON values.
Dimensions and frame rate must be positive, the frame interval nonempty, numeric
values finite and representable by the drawing stack, and layer ancestry acyclic.
The supported subset is 2D shape/null layers with paths, rectangles, ellipses,
groups, transforms, nonzero solid fills, linear/radial fills with two to four
ordered stops, and solid strokes with the default miter limit. Masking, mattes,
blend modes, repeaters/trims, gradient strokes, external assets/text, effects,
expressions, split positions/rotations, spatial position tangents and layer time
remapping, offset/stretch are refused. Layer transforms require rotation data.
Unknown shape operations are refused rather than lost.

Numerical smoke checks supplement structural admission. At emission, non-finite
frame arguments or computed drawing data return `LottieError` before any command
is appended; callers retain their existing list on refusal.

`kithara-ui` re-exports this crate as `kithara_ui::lottie` under its `render`
feature; the Lottie control picks an artwork by the name a document gives and
draws it through `emit`.

See [crate contracts](https://github.com/zvuk/kithara/wiki/kithara-ui-lottie) for detailed contracts, invariants, and internals.
