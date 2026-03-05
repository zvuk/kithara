# kithara-app

## Purpose
Application entrypoints and binaries for local demo/use.

## Owns
- CLI mode dispatch
- Desktop and TUI binary wiring
- App-level bootstrapping around shared engine crates

## Integrates with
- `kithara`, `kithara-ui`, `kithara-tui`

## Notes
- This crate is a consumer/composition layer, not core runtime logic.
