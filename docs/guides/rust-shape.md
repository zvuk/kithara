# Rust Shape

Use this when code starts to look like C with Rust syntax.

## Standard Traits First

Before adding a named helper, check whether the shape belongs to a standard trait:

- `From`, `TryFrom`, `FromStr`
- `Display`, `Default`
- `Iterator`, `IntoIterator`, `FromIterator`
- `Read`, `Write`

Avoid inherent `to_string`, ad-hoc `from_*`, manual parser APIs, and custom
collection builders when the trait expresses the contract.

## Loop Shape

- Use iterator adapters for filtering, mapping, searching, and folding.
- Use `for` when the body owns side effects, early exit, ordered protocol steps,
  or stateful coordination.
- Avoid accumulator loops, `for` plus `if` plus `push`, flag loops, parallel
  collection passes, and manual `match` forwarding instead of `?`.

```rust
let active: Vec<_> = items
    .iter()
    .filter(|item| item.is_active())
    .map(Item::id)
    .collect();
```

## Types And Ownership

- Carry domain meaning with newtypes, enums, config, or builders instead of bool
  flags, primitive spreads, or loose strings.
- Choose the API ownership boundary deliberately. Do not clone only to satisfy the
  compiler.
- Prefer typed errors and `?`; manual `match` is for domain work, not forwarding.

```rust
let media = parse_media(input)?;
let stream = open_stream(media)?;
```

```rust
let session = Session::new(SessionConfig {
    timeout,
    cancel,
    pools,
});
```
