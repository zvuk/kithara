# Kanban — `kithara-assets`

Этот документ содержит задачи, относящиеся **только** к сабкрейту `crates/kithara-assets`.

Общие инструкции (обязательная преамбула к каждой задаче): см. `kithara/docs/kanban-instructions.md`.

---

## Цель

`kithara-assets` — обязательный persistent disk **assets store** (не “blob cache”), который:

- хранит дерево ресурсов на диске (HLS требует tree-like layout),
- отдаёт *логические ресурсы* по ключам (а не “готовые байты целиком”):
  - **Streaming** (большие ресурсы): random-access `write_at/read_at` + `wait_range`,
  - **Atomic** (маленькие файлы: playlists/keys/metadata/index): whole-object `read/write` с crash-safe replace (temp → rename),
- обеспечивает lease/pin для активных asset’ов,
- обеспечивает eviction по общему размеру (bytes) с LRU по asset,
- имеет best-effort глобальный индекс `_index/state.json`:
  - индекс **не** является источником истины,
  - при необходимости восстанавливается по файловой системе.

---

## Публичный контракт (нормативно)

Публичный контракт `kithara-assets` — это **только** трейт:

- `Assets` (см. `crates/kithara-assets/src/lib.rs`)

Важное правило для агентов:
- всё, что является частью “обязательного контракта” — должно быть выражено в `Assets`;
- всё, что не является частью контракта — не должно быть `pub` (только internal).

---

## Компоненты (контракт и ответственность)

### 1) `Assets` (facade)

Нормативный контракт (актуально):

- открывает ресурсы по ключам:
  - `open_streaming_resource(key, cancel) -> AssetResource<StreamingResource>`
  - `open_atomic_resource(key, cancel) -> AssetResource<AtomicResource>`
- глобальный индекс:
  - `open_index_resource(cancel) -> AssetResource<AtomicResource>`
  - `load_index(cancel) -> AssetIndex`
  - `store_index(cancel, &AssetIndex)`

### 2) Ключи (внешний контракт)

`kithara-assets` **не формирует** пути/ключи.

Ключи задаются вызывающими слоями (`kithara-file`, `kithara-hls`):

- `ResourceKey { asset_root: String, rel_path: String }`
- маппинг на диск:
  - `<cache_root>/<asset_root>/<rel_path>`

Нормативные ограничения безопасности:
- запрещены абсолютные пути,
- запрещены `..`,
- запрещены пустые сегменты.

### 3) Индекс `_index/state.json` (best-effort)

- один глобальный индекс:
  - `asset_root = "_index"`
  - `rel_path = "state.json"`
- индекс:
  - не держится “открытым”,
  - не является source of truth,
  - может быть удалён/потерян и восстановлен из FS.

### 4) Auto-pin (lease) semantics (нормативно)

Все ресурсы, открытые через `Assets`, автоматически **pin**’ят `asset_root` на время жизни хэндла:

- тип-хэндл: `AssetResource<R>`
- pin реализован как RAII guard внутри `AssetResource`
- drop `AssetResource` => best-effort release pin

Нормативное ожидание:
- eviction никогда не должен удалять pinned `asset_root`.

---

## Инварианты (обязательные)

- **FS is source of truth**: файловая система определяет фактическое существование объектов.
- `kithara-assets` не должен “подменять” существование ресурсами в RAM-only метаданных.
- Crash-safe запись маленьких файлов (`AtomicResource`): temp file → rename.
- `StreamingResource` обеспечивает ожидание диапазонов (`wait_range`) и **не выдаёт ложный EOF**.
- Lease/pin:
  - pinned assets никогда не удаляются eviction’ом.
- Eviction:
  - лимит по общему размеру (bytes),
  - LRU по asset_root,
  - eviction не удаляет ресурсы, которые находятся в процессе наполнения (in-progress).

---

## `kithara-assets` — Tasks

### A) README (contract-first)

- [ ] Add crate README (`crates/kithara-assets/README.md`) describing:
  - `Assets` contract (methods + semantics),
  - key layout `<root>/<asset_root>/<rel_path>` (and safety rules),
  - `AssetResource<R>` auto-pin semantics,
  - atomic vs streaming resources (who uses which),
  - global index `_index/state.json`: best-effort, not source-of-truth, rebuild strategy.

Required tests:
- N/A (docs-only).

### B) Remove legacy blob-cache design (hard cut)

Нормативно: не сохраняем обратную совместимость.

- [x] Remove old “blob cache” API & modules (CachePath/Store/FsStore/IndexStore/LeaseStore/EvictingStore) from crate codebase
- [x] Ensure `kithara-assets` does not perform ad-hoc FS operations beyond opening resources

Required tests:
- N/A (structural cleanup), but keep existing tests compiling.

### C) Lease/pin — finalize semantics + tests

- [x] Auto-pin by default: any opened resource pins `asset_root` via `AssetResource<R>`
- [ ] Add explicit introspection hooks (internal/test-only):
  - `pin_count(asset_root) -> u64` (internal)
  - `is_pinned(asset_root) -> bool` (internal)
- [ ] Define and document (README) the best-effort unpin behavior (async drop) and what callers may assume

Required tests (unit/integration inside `crates/kithara-assets/tests/`):
- [ ] `lease_autopin_atomic_resource_increments_pin_count`
  - open `open_atomic_resource` twice for same `asset_root`
  - assert pin_count==2
- [ ] `lease_autopin_streaming_resource_increments_pin_count`
  - open `open_streaming_resource` twice
  - assert pin_count==2
- [ ] `lease_drop_decrements_pin_count_best_effort`
  - open two resources, drop one
  - wait until pin_count decreases (with bounded retry/timeout)
- [ ] `lease_is_keyed_by_asset_root_not_rel_path`
  - open two different `rel_path` under same asset_root
  - assert pin_count increments under the same key
- [ ] `lease_different_asset_root_are_independent`

### D) Eviction (LRU + max bytes) — new architecture

Goal: implement eviction that works with the resource-tree layout and respects leases.

- [ ] Define eviction policy contract (README + code):
  - max bytes global (configurable),
  - LRU by `asset_root`,
  - never evict pinned assets,
  - never evict in-progress resources.
- [ ] Implement `ensure_space(bytes_needed)` that can evict multiple assets.
- [ ] Implement best-effort LRU touch hook:
  - touch on `open_*_resource` (and optionally on reads via index, but keep it cheap)
- [ ] Decide how to identify “in-progress” safely (minimal viable):
  - conservative: only evict entries explicitly marked Ready in the global index,
  - otherwise treat as in-progress and skip.

Required tests (integration tests in `crates/kithara-assets/tests/` with temp dirs; no network):
- [ ] `evict_respects_pins_never_deletes_pinned_asset_root`
  - create two asset_roots with files
  - open a resource under one asset_root (pins it)
  - call ensure_space that would normally evict that asset
  - assert pinned asset files remain
- [ ] `evict_can_delete_multiple_asset_roots_to_free_space`
- [ ] `evict_lru_orders_by_last_access`
  - touch A, touch B, then touch A => B should be evicted first
- [ ] `evict_does_not_touch_in_progress`
  - mark asset_root as InProgress in index
  - ensure_space should not delete it even if over limit
- [ ] `evict_ignores_missing_index` (best-effort)
  - delete index file
  - eviction still works conservatively (may evict only clearly safe assets)

### E) Index (`_index/state.json`) — best-effort, rebuildable

- [x] Read/write via `AtomicResource` only (no ad-hoc fs writes)
- [ ] Define index schema versioning rules (when to bump, how to handle unknown versions)
- [ ] Implement `rebuild_index_from_fs()` best-effort:
  - scan `<cache_root>/**` excluding `_index/**`
  - derive entries from file metadata (size, paths)
  - set conservative statuses (e.g., `Ready` unless explicitly known otherwise)
- [ ] Make index read robust:
  - missing => default empty
  - invalid JSON => default empty (and optionally schedule rebuild)

Required tests:
- [ ] `index_missing_returns_default`
- [ ] `index_invalid_json_returns_default`
- [ ] `index_roundtrip_store_then_load`
- [ ] `index_rebuild_skips__index_directory`
- [ ] `index_rebuild_discovers_files_and_records_sizes`

---

## Notes (for agents)

- Этот крейт — **дерево ресурсов** + политики (lease/evict/index).
- Никаких сетевых запросов, HLS логики, или “range fetching” здесь быть не должно.
- Для small files используем `AtomicResource` из `kithara-storage`.
- Для streaming используем `StreamingResource` из `kithara-storage`.
- Любой новый публичный метод должен быть частью `Assets` трейта.