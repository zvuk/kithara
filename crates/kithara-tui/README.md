<div align="center">
  <img src="../../logo.svg" alt="kithara" width="300">
</div>

<div align="center">

[![CI](https://github.com/zvuk/kithara/actions/workflows/ci.yml/badge.svg)](https://github.com/zvuk/kithara/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](../../LICENSE-MIT)

</div>

# kithara-tui

Workspace TUI crate (`publish = false`) for terminal playback control and diagnostics.

## Responsibilities

- Terminal session lifecycle (`UiSession`) with raw mode + inline viewport
- Dashboard rendering (`Dashboard`) with queue/progress/volume/crossfade status
- Tracing initialization adapted for raw-mode terminals (`init_tracing`)

## Run

```bash
cargo run -p kithara-app --bin kithara-tui -- <TRACK_URL_1> <TRACK_URL_2>
```

## Control flow

```mermaid
flowchart LR
    keys["Keyboard + resize"] --> runner["kithara-app::tui_runner"]
    runner --> session["kithara-tui::UiSession"]
    runner --> dash["kithara-tui::Dashboard"]
    runner --> player["kithara::PlayerImpl"]
    player --> events["Player/Engine/Source events"] --> runner
```

## Integration

Used by `kithara-app` in TUI mode. This crate is intentionally UI-only: playback state, seek, volume, and track switching are delegated to `kithara` player APIs.
