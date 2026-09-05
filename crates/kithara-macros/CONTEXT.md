# kithara-macros - Context

## Ownership

This crate owns the shape of a configuration document. `#[derive(Patch)]` is the
only way a `<X>Patch` type enters the workspace: a hand-written patch struct or
a hand-written merge is a defect, not a style choice.

The crate owns no runtime state and depends on nothing but `syn`, `quote`, and
`proc-macro2`. The generated code names `::serde` and `::core` only, so a
consuming crate needs `serde` and nothing else.

## What The Derive Emits

From a configuration struct, taking every field not carrying `#[patch(skip)]`,
the derive emits two items in the same module:

- the patch struct, named after the configuration with a `Patch` suffix,
  carrying the configuration's own visibility, deriving `Clone`, `Debug`,
  `Default` and `Deserialize`, and attributed `#[serde(default,
  deny_unknown_fields)]` and `#[non_exhaustive]`;
- an inherent `apply` on the configuration itself, taking that patch by value
  and repeating the configuration's own generics and where-clause.

The patch carries no generic parameters of its own. That is the whole reason the
derive exists: `struct-patch`, the crate this replaced, copies a struct's
generics onto the patch it generates, so a patch of a generic configuration
whose generic-carrying fields are skipped has a type parameter no field uses and
does not compile. Every configuration in this workspace is generic over its
pools, its stream, or its resampler backend, and none of those is a document
key.

## Field Mapping

- A field of type `T` becomes `Option<T>`; the merge writes it when the document
  named it.
- A field already of type `Option<T>` stays `Option<T>`, so a document names the
  value bare. An absent key is the only way to leave the caller's value
  standing: a patch cannot clear a field back to `None`.
- `#[patch(nested)]` makes the patch field `<T>Patch` and the merge recurse.
  Nesting is declared, not inferred, so a document's shape is readable from the
  configuration alone.
- `doc` and `cfg` attributes carry over to the patch field, and a `cfg` also
  gates the merge statement, so a feature-gated field stays gated on both sides.

## Security Contract

A generated patch is `Deserialize` and never `Serialize`. By the time a document
is typed its `$ENV` references are resolved, so the patch holds secrets in the
clear; serializing one would write them out. The derive emits no `Serialize`,
and adding one to a configuration must not add one to its patch.
