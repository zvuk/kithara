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

Add `Source::FACTORY` to the app's source list; it mounts the source from
its `sources.zvuk` entry. `Source::registered` builds the same registration
over services the caller supplies.

```rust
use kithara_app_library::Factory;

const FACTORIES: &[Factory] = &[kithara_app_zvuk::Source::FACTORY];
```

### Configuration

The source owns the schema of its entry. `user_agent` is the client identity
every request carries; it is a literal or an environment reference that the
document resolves like any other. A null or absent entry mounts no source.

```yaml
sources:
  zvuk:
    user_agent: <client identity>
```

### Account

The account connects through Zvuk's device sign-in: Connect requests a code,
opens its confirmation page in the system browser and polls, at most every two
seconds, until the code is confirmed, expires or is cancelled. The access token lives
under the key `zvuk` of the app's secret store: the `secrets` section of the
configuration overlay, or with `keystore` the operating system's store under the
service `kithara`. A stored token starts the account signed in; a build without
`keystore` also takes one written by hand as `secrets: { zvuk: <token> }`.
Disconnect asks Zvuk to revoke the session and removes the stored token whatever
Zvuk answers; a code confirmed while the store refuses the write is revoked
again.

The account task is the single writer of the token. Catalogue requests and the
`zvuk.com` key grant read it per request; without a token the catalogue sends
no request. The token stays out of `Debug` output, errors and logs.

Catalogue reads keep the shared client's retry policy; like and unlike requests
go out once.

See [library sources](https://github.com/zvuk/kithara/wiki/kithara-app#library-sources)
for the source contract.

### Streaming

Adaptive streaming in the Zvuk settings section chooses the initial mode for
newly loaded tracks: AUTO when enabled, the fixed initial stream variant when
disabled. A deck's AUTO or quality selection governs its loaded track; changing
the source default leaves those tracks alone. Loading an already queued source URL
selects its existing resource and preserves its quality.

The native application stores the preference in `kithara/zvuk.json` under the
operating system's user configuration directory. Writes finish before the
switch accepts its new value; a failure is shown and preserves the accepted
value. The sidecar does not modify the secret overlay. A caller supplying
`Source::registered` without `settings_path` explicitly gets a session preference.
