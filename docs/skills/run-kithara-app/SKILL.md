---
name: run-kithara-app
description: Build and run Kithara. Default target is the desktop app (kithara-app, GUI). Use the iOS or Android sections ONLY when the user explicitly asks for that platform.
---

## Desktop (default)

```bash
cargo run -p kithara-app -- --mode gui
```

Add track paths/URLs as extra args to play specific tracks; without any, it
uses the built-in defaults.

## iOS — only if explicitly requested

```bash
just apple demo
```

## Android — only if explicitly requested

```bash
just android build
cd android && ./gradlew :example:installDebug
```
