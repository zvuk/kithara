<div align="center">
  <img src="../../logo.svg" alt="kithara" width="300">
</div>

<div align="center">

[![CI](https://github.com/zvuk/kithara/actions/workflows/ci.yml/badge.svg)](https://github.com/zvuk/kithara/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](../../LICENSE-MIT)

</div>

# kithara-ui

Workspace GUI crate (`publish = false`) implementing the desktop player UI with `iced`.

## Responsibilities

- GUI state model (`state.rs`) and message protocol (`message.rs`)
- Elm-style update loop (`update.rs`)
- Declarative widget tree and styling (`view.rs`, `theme.rs`, `icons.rs`)
- Playlist controls + seek/volume/EQ/crossfade UI over shared `PlayerImpl`

## Run

```bash
cargo run -p kithara-app --bin kithara-gui -- <TRACK_URL_1> <TRACK_URL_2>
```

## Data flow

```mermaid
flowchart LR
    input["User input"] --> msg["Message enum"] --> update["update(state, message)"]
    update --> player["kithara::PlayerImpl"]
    update --> state["Kithara state"]
    state --> view["view(state)"]
    tick["100ms Tick subscription"] --> update
```

## Integration

- Consumed by `kithara-app` GUI binary.
- Uses `kithara` (file/hls playback) and `iced`.
- Shares playback behavior with TUI/WASM because control operations are delegated to `PlayerImpl`.
