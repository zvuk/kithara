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
- имеет best-effort metadata в namespace `_index/*` (например `_index/pins.json` для pin table):
  - metadata **не** является источником истины,
  - metadata может быть удалена/потеряна (FS is source of truth).

---

## Публичный контракт (нормативно)

Публичный контракт `kithara-assets` выражен следующими публичными сущностями (см. `crates/kithara-assets/src/lib.rs`):

- `Assets` — базовый контракт открытия ресурсов по `ResourceKey` (без lease/pin).
- `LeaseAssets<A>` — декоратор над `A: Assets`, добавляющий lease/pin и возвращающий `AssetResource<_, _>` с RAII guard.
- `DiskAssetStore` — disk-backed реализация `Assets` (маппинг `ResourceKey` → реальные storage ресурсы).
- `EvictAssets<A>` — декоратор над `A: Assets`, добавляющий best-effort eviction (LRU/quota) на уровне asset_root.
- `EvictConfig` — конфигурация eviction (max_assets + max_bytes).
- `AssetStore` — type alias: `LeaseAssets<EvictAssets<DiskAssetStore>>`.
- `asset_store(root_dir, EvictConfig) -> AssetStore` — конструктор для ready-to-use композиции (free function, т.к. `AssetStore` это alias).

Важное правило для агентов:
- всё, что является частью “обязательного контракта” — должно быть выражено через эти публичные сущности (прежде всего `Assets` + `LeaseAssets`);
- всё, что не является частью контракта — не должно быть `pub` (только internal).

---

## Компоненты (контракт и ответственность)

### 1) `Assets` (base contract)

Нормативный контракт (актуально):

- открывает ресурсы по ключам **без** lease/pin (lease — ответственность декоратора):
  - `open_streaming_resource(key, cancel) -> AssetsResult<StreamingResource>`
  - `open_atomic_resource(key, cancel) -> AssetsResult<AtomicResource>`
- предоставляет атомарные ресурсы для маленького состояния декораторов (best-effort indexes):
  - `open_pins_index_resource(cancel) -> AssetsResult<AtomicResource>`
  - `open_lru_index_resource(cancel) -> AssetsResult<AtomicResource>`
- предоставляет хук для удаления целого asset (для eviction):
  - `delete_asset(asset_root, cancel) -> AssetsResult<()>`

Нормативно:
- `Assets` не “придумывает” пути и не занимается string-munging; ключи приходят снаружи.
- ресурсы индексов (`open_pins_index_resource`, `open_lru_index_resource`) должны быть исключены из pinning декораторами (иначе рекурсия).

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

### 3) Best-effort metadata (через `_index/*`)

На текущий момент в крейте зафиксировано best-effort состояние для lease/pin:

- pin table (persisted):
  - `asset_root = "_index"`
  - `rel_path = "pins.json"`
  - запись/чтение pin table — только через `Assets::open_pins_index_resource(...)` как `AtomicResource`.

Нормативно:
- metadata не является source of truth (FS is source of truth),
- metadata может быть удалена/потеряна (higher layers должны уметь восстановиться через повторное pin при открытии ресурсов).

### 4) Auto-pin (lease) semantics (нормативно)

Все ресурсы, открытые через декоратор `LeaseAssets<A>`, автоматически **pin**’ят `asset_root`
на время жизни хэндла:

- тип-хэндл: `AssetResource<R, _>` (внутри хранит RAII guard)
- pin реализован в `LeaseAssets`: перед возвратом хэндла создаётся lease guard
- drop `AssetResource` => best-effort release pin (асинхронно)

Нормативные свойства pin table:
- in-memory индекс: `HashSet<String>` (уникальные asset_root; без refcount),
- каждое добавление/удаление pin приводит к немедленному best-effort persisted snapshot через `AtomicResource`.

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

- [x] Add crate README (`crates/kithara-assets/README.md`) describing:
  - `Assets` + `LeaseAssets` contracts (methods + semantics),
  - key layout `<root>/<asset_root>/<rel_path>` (and safety rules),
  - `LeaseAssets` auto-pin semantics (`AssetResource<R, _>` with RAII guard),
  - atomic vs streaming resources (who uses which),
  - best-effort metadata under `_index/*` (today: pin table `_index/pins.json`), FS is source of truth.

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
- [x] Define and document (README) the best-effort unpin behavior (async drop) and what callers may assume

Notes:
- В текущей реализации pin table — это `HashSet<String>` (уникальные asset_root; без refcount).
- Поэтому “pin_count==2” для двух одновременно открытых хэндлов **не является частью контракта** и не тестируется.

Required tests (unit/integration inside `crates/kithara-assets/tests/`):
- [x] Covered by integration tests:
  - `mp3_single_file_atomic_roundtrip_with_pins_persisted`
  - `hls_multi_file_streaming_and_atomic_roundtrip_with_pins_persisted`

### D) Eviction (LRU + max bytes) — new architecture

Goal: implement eviction that works with the resource-tree layout and respects leases.

- [x] Define eviction policy contract (README + code):
  - max bytes global (configurable),
  - max assets global (configurable),
  - LRU by `asset_root`,
  - never evict pinned assets,
  - eviction runs only when a new `asset_root` is created (no background task).
- [x] Implement `ensure_space(...)` that can evict multiple assets.
- [x] Implement best-effort LRU touch hook:
  - touch on `open_*_resource` (keep it cheap)

Not implemented (by design / TBD):
- [ ] Decide how to identify “in-progress” safely (minimal viable):
  - conservative: only evict entries explicitly marked Ready in the global index,
  - otherwise treat as in-progress and skip.

Required tests (integration tests in `crates/kithara-assets/tests/` with temp dirs; no network):
- [x] `eviction_max_assets_skips_pinned_assets`
- [x] `eviction_max_bytes_uses_explicit_touch_asset_bytes`
- [ ] (Optional / future) `evict_lru_orders_by_last_access`
- [ ] (Optional / future) `evict_does_not_touch_in_progress`
- [ ] (Optional / future) `evict_ignores_missing_index` (best-effort)

### E) Metadata (`_index/*`) — best-effort, rebuildable

- [x] Read/write via `AtomicResource` only (no ad-hoc fs writes)
- [ ] Define metadata schema versioning rules (when to bump, how to handle unknown versions)
- [x] Make metadata read robust:
  - missing/empty => default empty
  - invalid JSON => default empty
- [ ] (Optional / if needed) Implement best-effort rebuild strategy for metadata where applicable.

Required tests:
- [x] `pins_index_missing_returns_default`
- [x] `pins_index_invalid_json_returns_default`
- [x] `pins_index_roundtrip_store_then_load`

---

## Notes (for agents)

- Этот крейт — **дерево ресурсов** + политики (lease/evict/index).
- Никаких сетевых запросов, HLS логики, или “range fetching” здесь быть не должно.
- Для small files используем `AtomicResource` из `kithara-storage`.
- Для streaming используем `StreamingResource` из `kithara-storage`.
- Любой новый публичный метод должен быть частью `Assets` трейта.
