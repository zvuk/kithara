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

A source exports a `Factory` that returns a `Registration`. It receives shared
services in `Environment` and its configuration and cancellation token in
`Context`. Use `Registration::fill` to add a page to `app-library/pages`.
The app builds the source after loading the text catalog.

See [library sources](https://github.com/zvuk/kithara/wiki/kithara-app#library-sources)
for the source contract.
