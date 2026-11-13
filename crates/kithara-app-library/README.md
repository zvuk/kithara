<div align="center">

<img src="https://raw.githubusercontent.com/zvuk/kithara/main/logo.svg" alt="kithara" width="300">

</div>

<div align="center">

[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](https://github.com/zvuk/kithara/blob/main/LICENSE-MIT)

</div>

# kithara-app-library

Workspace crate (`publish = false`) defining library sources for `kithara-app`.
Sources provide tree branches, rows, pages, and UI endpoints. The app renders
these and routes reads and writes to the owning source.

## Usage

A source exports a `Factory` that returns a `Registration`. It receives the
services the app shares with plugins in `Environment`, and its configuration
and cancellation token in `Context`. The context may also supply the source's
native preference file; an absent path requests session-only preferences. `Secrets::native` keeps secrets in the
`secrets` section of the configuration overlay it is given, or with the
`keystore` feature in the operating system's store; without either, such as in
the browser, every call returns `SecretError::Unsupported`.

Use `Registration::fill` to add a document to an app collection, and
`Registration::key_access` to grant the source's token to key requests of one
domain. The app builds the source after loading the text catalog.

See [library sources](https://github.com/zvuk/kithara/wiki/kithara-app#library-sources)
for the source contract.
