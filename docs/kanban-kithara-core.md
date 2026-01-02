# Kanban — `kithara-core`

Этот документ содержит задачи, относящиеся **только** к сабкрейту `crates/kithara-core`.

Общие инструкции (обязательная преамбула к каждой задаче): см. `kithara/docs/kanban-instructions.md`.

---

## Цель

`kithara-core` intentionally minimal: только identity и базовые ошибки. Никаких настроек кэша/ABR/HLS/сети — это живёт в соответствующих крейтах.

---

## Выполненные задачи (по git истории)

Сопоставление сделано по коммитам:
- `132f181` — `kithara-core` (vibe-kanban 828a28a9)
- `a767617` — `kithara-core — порт сценариев идентичности (tests)` (vibe-kanban f8a79d17)
- `a6995f9` — `kithara-core — refactor (module boundaries; keep public contract)` (vibe-kanban ab2a7beb)

---

## Sanity check (после ужесточения fmt+clippy)

- [ ] Run `cargo fmt` and ensure no formatting diffs remain
- [ ] Run `cargo test -p kithara-core`
- [ ] Run `cargo clippy -p kithara-core` and make it clean (no warnings/errors) under workspace lints

---
