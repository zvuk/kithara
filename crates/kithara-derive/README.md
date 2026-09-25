<div align="center">

<img src="https://raw.githubusercontent.com/zvuk/kithara/main/logo.svg" alt="kithara" width="300">

</div>

<div align="center">

[![crates.io](https://img.shields.io/crates/v/kithara-derive.svg)](https://crates.io/crates/kithara-derive)
[![docs.rs](https://docs.rs/kithara-derive/badge.svg)](https://docs.rs/kithara-derive)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](https://github.com/zvuk/kithara/blob/main/LICENSE-MIT)

</div>

# kithara-derive

Proc-macro crate for Kithara's production code. It generates configuration,
bounded-value, event, model-conversion, typestate, vocabulary, and UI trait
implementations while their product types remain in the owning crates.

The crate has no default features. Enable each derive explicitly with its
matching feature. The available features are `built-default`, `config`, `control`,
`control-painter`, `enum-str`, `event`, `mirror`, `node-control`, `patch`,
`phase`, `ranged`, `retained`, `skin-walk`, `variants`, and `view-control`.
The `event` feature exports both `Event` and `EventSet` because they form one
event contract.

## Usage

```rust
use kithara_derive::Patch;

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

Missing document keys leave the current value unchanged. A present value sets
it; explicit `null` clears an `Option` and is rejected for a required field.
Optional patch fields therefore carry `Option<Option<T>>`: `None` is unchanged,
`Some(None)` clears, and `Some(Some(value))` sets. Validation still runs before
committing the staged configuration. A null value never means reset to defaults.

### Bounded Scalars

`Ranged` accepts one attribute on a concrete numeric tuple newtype:
`#[ranged(min = <literal>, max = <literal>, default = <literal>, clamp)]`.
The bounds are required; `default` and `clamp` are optional. Bounds use float
literals for `f32`/`f64` and integer literals for integer fields, optionally
negated. Generics, repeated keys, and bounds outside their declared order are
refused.

```rust
use kithara_derive::Ranged;

#[derive(Clone, Copy, Debug, PartialEq, Ranged)]
#[ranged(min = -24.0, max = 6.0, default = 0.0, clamp)]
struct Gain(f32);

assert_eq!(Gain::from(f32::NAN), Gain::DEFAULT);
assert_eq!(Gain::checked(7.0), None);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Ranged)]
#[ranged(min = 0, max = 100, default = 100)]
struct Share(u8);

assert_eq!(Share::checked(101), None);
assert_eq!(u8::from(Share::default()), 100);
```

`checked` and `Deserialize` always refuse invalid values. Only `clamp` adds
`From<primitive>`; it requires `default`, which receives a floating-point NaN.
Without `default`, neither `DEFAULT` nor `Default` exists. The inverse `From`
always unwraps the value. Declaring crates must depend on `serde`; `Serialize`
is never generated.

### Events

`Event` implements the event marker for a concrete struct or enum. It refuses
unions and all type, lifetime, or const parameters. `EventSet` accepts a nonempty
concrete enum whose variants each wrap one event type in a tuple field.

```rust
use kithara_derive::{Event, EventSet};

#[derive(Clone, Debug, Event)]
enum PlaybackEvent {
    Started,
    Stopped,
}

#[derive(Clone, Debug, EventSet)]
enum ObserverEvent {
    Playback(PlaybackEvent),
}
```

Both expansions require a direct `kithara-events` dependency. `EventSet` emits
its own `From` implementations: do not also derive `derive_more::From`. Variant
order determines receive priority; closed channels do not block live members,
and lag is returned to the caller.

## Key Types

Derive macros:

- `#[derive(BuiltDefault)]` — implements `Default` through an existing builder
- `#[derive(Control)]` — implements the document-owned UI size contract
- `#[derive(ControlPainter)]` — forwards structural draw arguments
- `#[derive(EnumStr)]` — implements a closed enum string vocabulary
- `#[derive(Mirror)]` — converts between structurally matching product models
- `#[derive(NodeControl)]` — implements the retained UI host path
- `#[derive(Patch)]` — generates `<Struct>Patch` and `<Struct>::apply`
- `#[derive(Phase)]` — implements a closed typestate phase trait
- `#[derive(Ranged)]` — generates bounded scalar construction and deserialization
- `#[derive(Retained)]` — implements retained UI data updates
- `#[derive(SkinWalk)]` — traverses skin frame and text-role fields
- `#[derive(Variants)]` — exposes unit enum variants in declaration order
- `#[derive(ViewControl)]` — implements the immediate UI host path
- `#[derive(Event)]` — registers a concrete event type
- `#[derive(EventSet)]` — generates a consumer adapter over event channels

Field attributes:

- `#[patch(skip)]` — the field is not a document key. Naming it is refused by
  name rather than dropped silently.
- `#[patch(nested)]` — the field's own type has a patch; the document names it
  under a key of the same name and the merge recurses.
- `#[patch(humantime)]` — parses duration/time values, including optional values.
- `#[patch(attribute(...))]` — one attribute added to the generated patch field
  alone, for example `serde(with = "humantime_serde::option")` on a `Duration`.

## Integration

Used by every crate that owns a configuration a document may reach, and by
`kithara-app`, which deserializes the generated patches out of `app.yaml` and
applies them onto the configurations it built.

See [crate contracts](https://github.com/zvuk/kithara/wiki/kithara-derive) for the contract the generated code keeps.
