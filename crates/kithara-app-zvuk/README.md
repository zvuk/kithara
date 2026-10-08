<div align="center">

<img src="https://raw.githubusercontent.com/zvuk/kithara/main/logo.svg" alt="kithara" width="300">

</div>

<div align="center">

[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](https://github.com/zvuk/kithara/blob/main/LICENSE-MIT)

</div>

# kithara-app-zvuk

Workspace crate (`publish = false`) with the Zvuk library source: typed
catalogue operations (search, liked tracks, playlists, HLS stream resolution
and confirmed like and unlike requests) and the page it fills the
application's library pages with. The source implements `LibrarySource` from
`kithara-app-library` and compiles for native and WASM targets.

## Usage

The application lists the source's factory among the library sources its build
mounts. The factory builds the source from the document's `sources.zvuk` entry
over the application's shared `Environment` and fills `app-library/pages` with
its page.

```rust
use kithara_app_library::Factory;

const FACTORIES: &[Factory] = &[kithara_app_zvuk::Source::FACTORY];
```

### Configuration

The source owns the schema of its entry. Both fields are required, and each is
a literal or an environment reference that the document resolves like any other.
A null or absent entry mounts no source.

```yaml
sources:
  zvuk:
    user_agent: <client identity>
    auth_token: $KITHARA_DRM_PROD_AUTH_TOKEN
```

Catalogue requests carry `User-Agent` and `X-Auth-Token` from this entry and no
DRM provider header. Reads keep the shared client's retry policy; like and
unlike requests go out once, over a single-attempt handle on the same transport.

## Key Types

<table>

<tr><th>Type</th><th>Role</th></tr>

<tr><td><code>Source</code></td><td>The library source: its branch, rows, page and <code>FACTORY</code></td></tr>

<tr><td><code>Client</code></td><td>The catalogue client over a <code>Net</code> transport that a source is registered with</td></tr>

<tr><td><code>Config</code></td><td>The <code>sources.zvuk</code> entry</td></tr>

</table>

See [library sources](https://github.com/zvuk/kithara/wiki/kithara-app#library-sources)
for the source contract.
