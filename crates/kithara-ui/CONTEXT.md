# kithara-ui - Context

Detailed contracts and invariants for the kithara-ui crate; the README remains the overview.

## Compiled String Ownership

Owner decision (b), 2026-07-19: every string retained by the compiled tree is interned in one
plain bounded `String` arena owned by `CompiledUi`. `UiConfig.max_arena_bytes` caps the string
buffer; growth uses `try_reserve`, and either the configured cap or an allocation failure returns
`UiDocError::ArenaFull`.

The compiled tree deliberately does not use kithara-bufpool. Its budget-charging `ensure_len`
requires `Default + Clone`, which `ExpandedNode` cannot provide. Growing a pooled structural
`Vec` through `push` would therefore bypass budget charging and make the budget inaccurate. A
pooled string buffer would also make `CompiledUi` non-`Clone` while occupying a churn-pool slot
for the preset lifetime. Bufpool remains the intended tool for the render hot path in a later
phase.

`InternId` is valid only within the `CompiledUi` that produced it. Recompilation rebuilds the
arena, so intern IDs must never be persisted in application messages or state. `ModularMsg`
continues to carry owned `String` paths, and hidden-module settings continue to store names.

`StrArena::resolve` is total. Spans describe whole strings appended to the buffer, so valid spans
land on UTF-8 boundaries and `String::get` resolves them without unsafe conversion. An unknown ID
or invalid span resolves to `""`; this is the documented handle behavior, not error recovery.

## Document And Compiled Layers

`BindingRef` and `PropValue<String>` are the serde document inputs. `Binding` and
`PropValue<InternId>` are their interned compiled forms. This is an explicit layer split, not a
second source of domain truth: validation runs on the substituted document forms before they are
interned.

The arena types live in `ids.rs` because they back compiled identifiers and strings, and keeping
them there preserves the crate's flat-directory budget.
