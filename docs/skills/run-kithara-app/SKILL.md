---
name: run-kithara-app
description: Build and run Kithara. Default target is the desktop app (kithara-app, GUI). Use the design-system gallery, iOS, or Android sections ONLY when the user explicitly asks for that target.
---

## Desktop (default)

```bash
cargo run -p kithara-app -- --mode gui
```

Add track paths/URLs as extra args to play specific tracks; without any, it
uses the built-in defaults.

## Design-system gallery — only if explicitly requested

```bash
cargo run -p kithara-ui --example gallery --features render
```

## iOS — only if explicitly requested

```bash
just apple demo
```

## Android — only if explicitly requested

```bash
just android build
cd android && ./gradlew :example:installDebug
```
