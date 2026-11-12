<div align="center">

<img src="https://raw.githubusercontent.com/zvuk/kithara/main/logo.svg" alt="kithara" width="300">

</div>

<div align="center">

[![crates.io](https://img.shields.io/crates/v/kithara-config.svg)](https://crates.io/crates/kithara-config)
[![docs.rs](https://docs.rs/kithara-config/badge.svg)](https://docs.rs/kithara-config)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](https://github.com/zvuk/kithara/blob/main/LICENSE-MIT)

</div>

# kithara-config

Derives for configuration types. A subsystem keeps the configuration that
governs it and reads its settings from there; `#[derive(Config)]` generates the
builder, getters, snapshot, validation, and live changes from one
`#[config(...)]` attribute, so none of that is written by hand.

## Usage

```rust
use kithara_config::Config;

#[derive(Config)]
#[config(fields(value, get(copy), builder(default)))]
pub struct Settings {
    retries: u32,
    #[config(skip = "owned runtime resource", get(ref))]
    resource: String,
}
```

### `#[config(...)]` options

On the type, `fields(...)` sets defaults that each field may override; a
field's explicit group replaces the inherited one. Conflicting options are
errors.

| Option | Effect |
| --- | --- |
| `value` / `nested` / `skip = "reason"` | Field role: a setting, a nested configuration, or a field kept out of the snapshot |
| `construction` | Unmarked fields are builder inputs that are consumed, not retained |
| `value(Type, expr)` | Expose a borrowed or internal field as an owned value |
| `get(ref)` / `get(copy)` / `get(skip)` | Getter that borrows, copies, or is not generated |
| `builder(default)`, `builder(default = v)`, `builder(skip)` | `bon` builder options per field; `builder(existing)` keeps a domain constructor's builder, `builder(none)` generates none |
| `check(error = E)` + `value, check = path` | Field checks; `validate_builder` makes `build()` return `Result` |
| `patch(...)` | How a configuration document patches the field |
| `live`, `live(owner)` | The field can change while the configuration runs |
| `debug`, `debug(skip)` | Derive `Debug`, leaving out marked fields |
| `wrap(default = v, with = f)` | Builder default and conversion for a wrapped field |
| `owner_access` | Generate `<Name>OwnerAccess` so any `ConfigOwner` exposes `get(ref)` fields |

`path_from_env(name)` reads an OS path from an environment variable for a
`builder(default = ...)` expression; unset or empty gives `None`.

## Key Types

- `Config` — the retained configuration; `values()` returns an owned
  snapshot (`<Name>Values`).
- `ConfigOwner` — `config()` on the type that owns a configuration; derive it
  with `#[config_owner(field)]`, `#[config_owner(Type, field.path)]`, or
  `#[config_owner(delegate(field))]`.
- `CheckedConfig` — `validated()` runs field checks in declaration order;
  document merges pass the same checks.
- `LiveConfig` — a `live` field becomes a variant of `<Name>Change`; `check`
  validates one change, `apply_change` applies it without allocating.
- `Configure` — implemented by an owner: `configure(change, at)` schedules a
  change, `settings()` returns the configuration as last applied. The derive
  adds `<Name>Control` (getters and `set_<field>`) and `<Name>Exec`.

A rejected change leaves the accepted configuration intact.

See the [workspace architecture](https://github.com/zvuk/kithara/wiki/kithara)
for domain ownership boundaries.
