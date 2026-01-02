# Kanban — `kithara-cache`

Этот документ содержит задачи, относящиеся **только** к сабкрейту `crates/kithara-cache`.

Общие инструкции (обязательная преамбула к каждой задаче): см. `kithara/docs/kanban-instructions.md`.

---

## Цель

`kithara-cache` — обязательный **persistent disk cache**, “tree-friendly” для HLS, с crash-safe записью и eviction по общему размеру (bytes), LRU по asset, и pin/lease для активного трека.

---

## Компоненты (контракт и ответственность) — напоминание

- `CachePath`: вычисление путей в кэше (layout), без утечек деталей наружу.
- `Store` / `KV` (internal): минимальный blob-интерфейс для слоёв (exists/open/put_atomic/remove_all).
- `FsStore`: базовый слой файловой системы (tree-friendly layout + atomic write).
- `IndexStore<S>`: индекс (например, `state.json`) + total_bytes/per-asset метаданные; **FS остаётся источником истины**.
- `LeaseStore<S>`: pin/lease (активный трек нельзя выкидывать).
- `EvictingStore<S, P>`: eviction по политике (LRU default), может удалить несколько assets.
- Facade: `AssetCache`: публичный API, который композирует слои и скрывает детали.

---

## Инварианты (обязательные)

- Crash-safe запись: temp file → `rename` (атомарная замена).
- Eviction лимитируется `max_bytes` по **всему кэшу**, а не “по файлу/ресурсу”.
- Можно удалить **несколько** assets, чтобы освободить место.
- Pinned/leased assets **никогда** не удаляются eviction’ом.
- `state.json` (если используется) — оптимизация/индекс, но **не** единственный источник истины: FS должна оставаться валидной после крэша/перезапуска.
- Кэшируем всё, что нужно для offline: playlists, segments, keys; ключи — **после обработки** (`processed keys`).

---

## `kithara-cache` MVP

- [x] FS layout + safe `CachePath`
- [x] `put_atomic` temp+rename + tests
- [x] `exists/open` via metadata + tests
- [x] `state.json` index load/save atomic + tests
- [x] `max_bytes` eviction loop (multi-asset) + tests
- [x] pin/lease + tests

---

## `kithara-cache` — Port legacy HLS persistence semantics (tests)

- [ ] Persistent cache warmup: after first run that reads at least N bytes from an HLS VOD session, the persistent cache root is non-empty; after second run (same root), it remains non-empty (do NOT assert “no network refetch”) + tests
- [ ] Persistent mode should create on-disk files after reading some bytes (smoke: read >0 then assert cache root has files) + tests
- [ ] Memory/offline-only resource caching should NOT create many on-disk segment files:
  - after reading >0 bytes, number of files on disk stays within a small bound (playlists/keys allowed, but not segment explosion) + tests
- [ ] (Prereq if missing) Add test utilities to count files recursively and assert non-empty dirs (test-only helpers) + tests

---

## `kithara-cache` — Layered architecture (decorators/generics; keep public contract)

- [x] Refactor internals into explicit layers/modules (base/index/lease/evict/policy) without changing the public API
- [x] Define internal minimal “store” contract for blob ops (exists/open/put_atomic/remove_all) + unit tests with a temp dir
- [x] Implement `FsStore` base layer (tree-friendly layout + atomic write) and make it pass the existing MVP tests
- [x] Implement `IndexStore<S>`: `state.json` atomic load/save + total_bytes/per-asset metadata; ensure FS remains source of truth
- [x] Implement `LeaseStore<S>`: `LeaseGuard`/pin semantics and lease file/touch behavior + tests
- [x] Implement `EvictingStore<S, P>` with policy trait `P` (LRU default) + tests:
  - evicts multiple assets until fit
  - never evicts pinned
  - updates index consistently
- [x] Wire `AssetCache` facade to compose layers (Fs + Index + Lease + Evict) while preserving the existing public contract and tests
- [ ] Add crate-level docs (`crates/kithara-cache/README.md`): layering, invariants, and what is source of truth

---

## `kithara-cache` — Refactor (layering-ready; keep public contract)

- [ ] Split internal modules to match responsibilities (base/index/lease/evict/policy) without changing public API + keep tests green
- [ ] Remove “misc utils dumping-ground”: move helpers into focused modules (e.g. `fs_layout`, `atomic_write`, `lru_index`) + keep tests green
- [ ] Add crate-level docs: invariants + “FS is source of truth” + what is stored in `state.json`

---

## Sanity check + починка после ужесточения fmt+clippy

- [x] Fix compilation errors in `kithara-cache` (duplicates, missing modules/symbols, broken impls)
- [x] Run `cargo fmt` and ensure no formatting diffs remain
- [x] Run `cargo test -p kithara-cache`
- [x] Run `cargo clippy -p kithara-cache` and make it clean (no warnings/errors) under workspace lints
