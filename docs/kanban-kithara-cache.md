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

## `kithara-cache` — Port legacy HLS persistence semantics (tests)

- [ ] Persistent cache warmup: after first run that reads at least N bytes from an HLS VOD session, the persistent cache root is non-empty; after second run (same root), it remains non-empty (do NOT assert “no network refetch”) + tests
- [ ] Persistent mode should create on-disk files after reading some bytes (smoke: read >0 then assert cache root has files) + tests
- [ ] Memory/offline-only resource caching should NOT create many on-disk segment files:
  - after reading >0 bytes, number of files on disk stays within a small bound (playlists/keys allowed, but not segment explosion) + tests
- [ ] (Prereq if missing) Add test utilities to count files recursively and assert non-empty dirs (test-only helpers) + tests

---

## `kithara-cache` — Refactor (layering-ready; keep public contract)

- [x] Split internal modules to match responsibilities (base/index/lease/evict/policy) without changing public API + keep tests green
- [x] Remove “misc utils dumping-ground”: move helpers into focused modules (e.g. `fs_layout`, `atomic_write`, `lru_index`) + keep tests green
- [x] Add crate-level docs: invariants + “FS is source of truth” + what is stored in `state.json`
