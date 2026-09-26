<div align="center">

<img src="https://raw.githubusercontent.com/zvuk/kithara/main/logo.svg" alt="kithara" width="300">

</div>

<div align="center">

[![crates.io](https://img.shields.io/crates/v/kithara-ui-input.svg)](https://crates.io/crates/kithara-ui-input)
[![docs.rs](https://docs.rs/kithara-ui-input/badge.svg)](https://docs.rs/kithara-ui-input)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](https://github.com/zvuk/kithara/blob/main/LICENSE-MIT)

</div>

# kithara-ui-input

Pointer, wheel, keyboard and input-method events in one toolkit-neutral shape,
and the recognizers that turn them into the gestures kithara-ui controls answer:
a click, a double click, a scalar drag with its wheel step, an item drag, a
carried placement, an edge span, a stepper. A recognizer is handed an event and
the box its control was laid out into, and answers with an `Outcome`: the value
it produced, whether the event goes on to the next control, and whether the
pointer now belongs to it. The `iced` and `masonry` features decode each
toolkit's events into that shape.

## Usage

```rust
use kithara_ui_draw::{Pt, Rect};
use kithara_ui_input::{Hit, Input, PointerPhase, mouse, recognizers::click};

let area = Rect { h: 20.0, w: 40.0, x: 0.0, y: 0.0 };
let at = Some(Pt { x: 10.0, y: 10.0 });
let press = Input::Pointer(mouse(PointerPhase::Down, at));
assert_eq!(click::on_input(press, &Hit::new(at, area)).value(), Some(()));
```

## Key Types

<table>

<tr><th>Type</th><th>Role</th></tr>

<tr><td><code>Input</code> / <code>Hit</code></td><td>One neutral event, and where the pointer stands against the control's box</td></tr>

<tr><td><code>Outcome</code></td><td>What a recognizer produced, whether the event propagates, and who owns the pointer</td></tr>

<tr><td><code>recognizers</code></td><td>The gestures a control answers, each with its own state</td></tr>

<tr><td><code>CursorShape</code> / <code>Hover</code></td><td>The cursor a control asks for while hovered or held</td></tr>

</table>

## Integration

`kithara-ui` re-exports this crate as `kithara_ui::interact` under its `render`
feature and turns on `iced` and `masonry` with its own features of the same
name; each host decodes its toolkit's events here and routes the neutral input
to the control under the pointer.

See [crate contracts](https://github.com/zvuk/kithara/wiki/kithara-ui-input) for detailed contracts, invariants, and internals.
