<div align="center">

<img src="https://raw.githubusercontent.com/zvuk/kithara/main/logo.svg" alt="kithara" width="300">

</div>

<div align="center">

[![crates.io](https://img.shields.io/crates/v/kithara-config.svg)](https://crates.io/crates/kithara-config)
[![docs.rs](https://docs.rs/kithara-config/badge.svg)](https://docs.rs/kithara-config)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](https://github.com/zvuk/kithara/blob/main/LICENSE-MIT)

</div>

# kithara-config

`Config` is the retained settings contract. A subsystem keeps the configuration
that governs its behavior and reads settings from it; `ConfigOwner::config`
exposes that canonical object. A mutable setting is changed in the retained
configuration, not in a second owner field. `LiveConfig` gives typed live
field changes a common API. Rejected changes leave the accepted configuration
intact.
`#[derive(ConfigOwner)]` implements the owner accessor from
`#[config_owner(field)]`. For a nested field, give its type and path as
`#[config_owner(ConfigType, field.path)]`; for a field that is itself an
owner, such as a live tracker, `#[config_owner(delegate(field))]` reads the
configuration that field owns.
With `#[config(owner_access)]`, fields marked `get(ref)` also produce a
`<Config>OwnerAccess` trait. Import
that trait to call the same getter on any `ConfigOwner` of that type; the method
borrows the retained configuration directly. For an exclusively owned mutable
configuration, `#[config_owner_mut]` on the owner derive adds `ConfigOwnerMut`
and its `apply_config_change` method. Shared and realtime owners keep their
domain application method so they can prepare and publish accepted changes.
Owners still decide when to prepare and publish derived state, especially across
realtime and thread boundaries. Buffers, counters, handles and observed results
remain operational state. `Config::values` returns an owned observation snapshot;
it does not become another mutable store or promise realtime safety.

`#[derive(Config)]` builds the whole configuration type from one `#[config(...)]`
attribute. Retained fields explicitly select `value`, `nested`, or `skip = "reason"`;
`construction` classifies unmarked fields as consumed inputs. For homogeneous
retained fields, `fields(value)` or `fields(nested)` sets their default role while
explicit field roles still override it.

`value(Type, expression)` projects a borrowed or internal field into an owned
public value. The derive generates a bon builder (`X::builder()`), whose
per-field options live in the field's `builder(...)` group and whose top-level
options live in the type's; `builder(skip)` or `builder(skip = value)` leaves a
field out of the builder. `get(ref)` borrows the original field and `get(copy)`
returns it through `Copy`; `get(skip)` disables an inherited getter.

`fields(...)` on the type accepts the same options as `config(...)` on a field:
roles, getters, `builder(...)`, `patch(...)`, `debug(skip)`, `live`, `check`,
and `wrap(...)`. Each explicit field facet overrides its inherited counterpart;
a group replaces the whole inherited group. A field's explicit builder or
wrapper replaces the inherited construction settings. Snapshot exclusion does not exclude a getter,
builder argument, or document patch: each facet declares its own policy.
Duplicate or conflicting options within one declaration are errors.

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

`#[config(validate_builder, check(error = Error))]` makes `build()` return
`Result<Self, Error>` through the field checks; `patch(validate = ..., error = ...)`
in place of `check(...)` does the same through one whole-configuration check.
`builder(existing)` keeps a domain constructor's bon builder when it must
consume inputs and retain only their prepared effective value. A fallible
`#[config(default)]` checks its declared defaults and treats their rejection
as a programming error.
`builder(none)` keeps serde-owned document schemas as retained configurations
without adding an unused programmatic constructor.
Standalone builders use `bon` directly. `Config` generates the builder for
configuration structs; a separate domain constructor annotated with `#[bon]`
remains independent of that derive.
`#[config(debug)]` derives `Debug` without the fields marked `debug(skip)`. For
a projected field stored in a wrapper, `wrap(default = value, with = Wrapper::new)`
derives the builder default and setter conversion. `Patch` reads the type's and
each field's `patch(...)` group.

`#[config(check(error = Error))]` declares what field checks refuse with;
`#[config(value, check = path)]` names a field's check, a function from the
value to `Result<value, Error>`. The derive implements `CheckedConfig`, whose
`validated` runs the checks in declaration order and validates nested
configurations whole; a document merge passes through the same gate.
`#[config(value, live)]` or `#[config(nested, live)]` makes a field change while
the configuration runs. Each live field becomes one variant of the generated
`<Name>Change` enum, and the derive implements `LiveConfig`: `check` passes a
change through its field's check alone, and `apply_change` assigns that field
alone without allocating. A nested live field carries the nested
configuration's change, and the parent's change converts from it, so two
nested live fields cannot share a type. `live(owner)` marks a value field whose change the
owner executes with its own operation; a nested live configuration cannot have
one.

An owner implements `Configure<<Name>Change>`: `configure(change, at)` checks one
change and hands it on for the moment `at` (`Default` is the nearest one), and
`settings()` returns the configuration as last applied. Addressing the trait by
the change type lets one owner configure several configurations. The derive
emits `<Name>Control`, implemented for every such owner: a getter per field with
an accessor, which reads `settings()`, and a `set_<field>` per live value field,
which calls `configure` for the nearest moment. A nested live field's getter
returns `Nested`, a `Configure` of the nested configuration whose changes reach
the owner as the parent's change. `<Name>Exec<Cx>` is the owner's side:
`exec` sends each `live(owner)` field to its `exec_<field>` method and every
other live field to `exec_live`.

The derive emits `<Name>Values` with public snapshot fields, preserving field
documentation. Resource generics stay on the original owner; snapshot types
must not depend on them. Domain constructors and methods remain responsible for
preparation and effects.

See the [workspace architecture](https://github.com/zvuk/kithara/wiki/kithara)
for domain ownership boundaries.
