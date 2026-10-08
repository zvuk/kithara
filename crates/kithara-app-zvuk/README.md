<div align="center">

<img src="https://raw.githubusercontent.com/zvuk/kithara/main/logo.svg" alt="kithara" width="300">

</div>

<div align="center">

[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](https://github.com/zvuk/kithara/blob/main/LICENSE-MIT)

</div>

# kithara-app-zvuk

Workspace crate (`publish = false`) providing the Zvuk library source and page.
Supports search, liked tracks, playlists, HLS stream resolution, and like/unlike
requests on native and WASM targets.

## Usage

Add `Source::FACTORY` to the app's source list. It reads `sources.zvuk` and
registers its page in `app-library/pages`.

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

See [library sources](https://github.com/zvuk/kithara/wiki/kithara-app#library-sources)
for the source contract.
