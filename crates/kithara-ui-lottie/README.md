<div align="center">

<img src="https://raw.githubusercontent.com/zvuk/kithara/main/logo.svg" alt="kithara" width="300">

</div>

<div align="center">

[![crates.io](https://img.shields.io/crates/v/kithara-ui-lottie.svg)](https://crates.io/crates/kithara-ui-lottie)
[![docs.rs](https://docs.rs/kithara-ui-lottie/badge.svg)](https://docs.rs/kithara-ui-lottie)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](https://github.com/zvuk/kithara/blob/main/LICENSE-MIT)

</div>

# kithara-ui-lottie

The Lottie artwork kithara-ui ships, read once per process, and the emitter
that draws one frame of any Lottie composition into the toolkit-neutral draw
list. A frame is drawn whole or refused whole: an artwork that asks for
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

`kithara-ui` re-exports this crate as `kithara_ui::lottie` under its `render`
feature; the Lottie control picks an artwork by the name a document gives and
draws it through `emit`.

See [crate contracts](https://github.com/zvuk/kithara/wiki/kithara-ui-lottie) for detailed contracts, invariants, and internals.
