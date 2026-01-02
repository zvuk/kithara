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

## `kithara-core` MVP

- [x] Define `AssetId` and canonicalization rules (+ tests)
- [x] Define `ResourceHash` rules (+ tests)
- [x] Define shared error scaffolding

---

## `kithara-core` — Port legacy identity scenarios (tests)

- [x] Canonicalization invariants for `AssetId` (URL without query/fragment) + tests:
  - ignores query
  - ignores fragment
  - stable across repeated parsing/formatting
- [x] Canonicalization invariants for `ResourceHash` (URL with query, without fragment) + tests:
  - includes query
  - ignores fragment
  - differs when query differs (same base URL)
- [x] (Prereq if missing) Add explicit error type(s) for invalid canonicalization inputs + tests:
  - invalid/unsupported URL shapes produce typed errors (no panics)

---

## `kithara-core` — Refactor (module boundaries; keep public contract)

- [x] Split into small modules if file grows: `asset_id`, `resource_hash`, `errors` (no behavior change) + keep tests green
- [x] Ensure no “settings creep”: options must remain outside `kithara-core` + add compile-time/doc guard (short comment or doc test)
- [x] Add crate-level docs: “what belongs here / what must not” (short, contract-level)

---