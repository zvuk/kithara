# Kithara: Анализ поддерживаемости и производительности на мобильных устройствах

## Общая оценка

Кодовая база kithara продемонстрировала высокий уровень инженерной культуры:
- 11 крейтов с чёткими зонами ответственности
- `#![forbid(unsafe_code)]` во всех файлах (44/44)
- 0 `TODO`/`FIXME`/`HACK` в продакшн-коде
- Единый стиль через `rustfmt.toml`, `clippy.toml`, `deny.toml`
- Продуманная система пулов буферов (`SharedPool`) для горячих путей

Ниже — конкретные рекомендации, сгруппированные по приоритету.

---

## Часть 1: Поддерживаемость (Maintainability)

### 1.1 [ВЫСОКИЙ] Аллокация `Vec<f32>` на каждый декодированный пакет

**Файл:** `kithara-decode/src/symphonia.rs:313`

```rust
let mut pcm = vec![0.0f32; num_samples];
decoded.copy_to_slice_interleaved(&mut pcm);
```

Каждый вызов `next_chunk()` создаёт новый `Vec<f32>`. При типичном размере 1024–4096 сэмплов на пакет и ~43 пакетов/сек (44100 Hz, 1024 фреймов) это ~43 аллокации/сек.

**Проблема поддерживаемости:** Существует глобальный `PcmPool` (`pcm_pool()`), но он не используется в самом декодере — только в `kithara-audio`. Это создаёт неявный контракт: "декодер аллоцирует, а вызывающий код управляет пулом". Такой контракт нигде не задокументирован и хрупок при рефакторинге.

**Рекомендация:**
- Передавать `PcmPool` в `SymphoniaDecoder` через конфигурацию
- Использовать `pool.get_with(|v| v.resize(num_samples, 0.0))` вместо `vec![]`
- Документировать в `kithara-decode/README.md`, что декодер использует пул

---

### 1.2 [ВЫСОКИЙ] Удерживание мьютекса при загрузке ресурса из кэша

**Файл:** `kithara-assets/src/cache.rs:94-104`

```rust
let mut cache = self.cache.lock();
if let Some(CacheEntry::Resource(res)) = cache.get(&cache_key) {
    return Ok(res.clone());
}
// Cache miss - load from base (still holding lock)
let res = self.inner.open_resource_with_ctx(key, ctx)?;
cache.put(cache_key, CacheEntry::Resource(res.clone()));
```

Лок удерживается во время I/O-операции (`open_resource_with_ctx`), которая может включать обращение к файловой системе.

**Рекомендация:** Использовать паттерн "check-release-load-recheck":
```rust
// 1. Проверить кэш
// 2. Отпустить лок
// 3. Загрузить ресурс
// 4. Захватить лок, вставить в кэш (проверив double-insert)
```

---

### 1.3 [СРЕДНИЙ] Синхронная персистенция пинов при каждом открытии ресурса

**Файл:** `kithara-assets/src/lease.rs:97-104`

```rust
let (snapshot, was_new) = {
    let mut pins = self.pins.lock();
    let was_new = pins.insert(asset_root.to_string());
    (pins.clone(), was_new)
};
if was_new {
    let _ = self.persist_pins_best_effort(&snapshot);
}
```

`pins.clone()` клонирует весь `HashSet<String>` при каждом новом пине. `persist_pins_best_effort` выполняет синхронный I/O.

**Рекомендация:**
- Дебаунсить персистенцию: записывать не чаще раза в N секунд или при shutdown
- Рассмотреть `tokio::spawn` для фоновой записи
- Использовать инкрементальный формат (append-only log) вместо полного переписывания

---

### 1.4 [СРЕДНИЙ] Размер LRU-кэша ресурсов захардкожен

**Файл:** `kithara-assets/src/cache.rs:61-62`

```rust
cache: Arc::new(Mutex::new(LruCache::new(
    NonZeroUsize::new(5).expect("capacity must be non-zero"),
))),
```

Магическое число `5` без конфигурируемости. В HLS-сценарии с init-сегментом и медиа-сегментом на вариант, 5 записей могут вызвать cache thrashing при 3+ вариантах.

**Рекомендация:**
- Вынести размер кэша в `AssetStoreBuilder`
- Рассмотреть адаптивный размер в зависимости от доступной памяти

---

### 1.5 [СРЕДНИЙ] Дублирование логики `wait_range` между HLS и Storage

`kithara-hls/src/source.rs:917-998` и `kithara-storage/src/resource.rs:444-507` оба реализуют паттерн "lock → check coverage → condvar wait". HLS-версия добавляет on-demand сегментную загрузку, но базовый цикл ожидания дублируется.

**Рекомендация:** Рассмотреть извлечение общего трейта или утилиты `WaitableRange` для единообразной реализации блокирующего ожидания данных.

---

### 1.6 [СРЕДНИЙ] `expect()` в продакшн-коде

**Файл:** `kithara-assets/src/cache.rs:62`

```rust
NonZeroUsize::new(5).expect("capacity must be non-zero")
```

Нарушает правило `unwrap_used = "deny"`. Хотя clippy `expect` и `unwrap` — разные линты, AGENTS.md явно запрещает оба в продакшн-коде.

**Рекомендация:** Заменить на `const`-инициализацию:
```rust
const CACHE_CAPACITY: NonZeroUsize = NonZeroUsize::new(5).unwrap(); // const context OK
// Или использовать NonZeroUsize::MIN и проверку
```

---

### 1.7 [НИЗКИЙ] Feature flag `perf` разбросан по 7 крейтам без единого контроля

Каждый крейт независимо определяет `perf = ["dep:hotpath"]`. Нет способа включить профилирование для всего workspace одной командой без перечисления `-p crate1 --features perf -p crate2 --features perf ...`.

**Рекомендация:** Добавить workspace-level feature в корневом `kithara`:
```toml
[features]
perf = ["kithara-decode/perf", "kithara-stream/perf", "kithara-bufpool/perf", ...]
```
Уже частично реализовано в `crates/kithara/Cargo.toml`, но стоит убедиться, что покрыты все крейты.

---

### 1.8 [НИЗКИЙ] Отсутствие CI-пайплайна для `cargo clippy` и `cargo fmt --check`

В `.github/workflows/` есть `coverage.yml` и `perf.yml`, но нет основного CI для проверки форматирования и линтинга. Это снижает уверенность в единообразии кода при PR.

**Рекомендация:** Добавить `ci.yml` с шагами:
```yaml
- cargo fmt --all --check
- cargo clippy --workspace -- -D warnings
- cargo test --workspace
- cargo doc --workspace --no-deps
```

---

## Часть 2: Производительность на мобильных устройствах

### 2.1 [КРИТИЧЕСКИЙ] Потребление памяти буферными пулами

**Файл:** `kithara-bufpool/src/global.rs:25,33`

```rust
BytePool::new(1024, 64 * 1024)   // до 1024 буферов × 64 КБ = 64 МБ
PcmPool::new(64, 200_000)         // до 64 буфера × 200 КБ ≈ 12 МБ
```

Суммарно пулы могут удерживать до ~76 МБ. На мобильных устройствах с ограничением 128–256 МБ на приложение это значительная часть бюджета.

**Рекомендации:**
- Сделать размеры пулов конфигурируемыми через `AudioConfig` / глобальные настройки
- Ввести платформенные пресеты: `Pool::mobile()` (256 byte bufs, 16 PCM bufs), `Pool::desktop()`
- Добавить API для принудительного сброса пулов при memory warning (`didReceiveMemoryWarning` на iOS / `onTrimMemory` на Android)
- Рассмотреть уменьшение `trim_capacity` для мобильных: 16 КБ вместо 64 КБ для байтовых буферов

---

### 2.2 [КРИТИЧЕСКИЙ] Качество ресемплера по умолчанию — `High` (256-tap sinc)

**Файл:** `kithara-audio/src/resampler.rs:37`

```rust
#[default]
High,  // 256-tap, cubic interpolation
```

256-tap sinc с кубической интерполяцией — тяжёлая операция для мобильного CPU, особенно при thermal throttling. На ARM-процессорах без мощного SIMD это может привести к пропускам аудио.

**Рекомендации:**
- Изменить дефолт на `Normal` (64-tap) или `Good` (128-tap) для мобильных платформ
- Использовать feature flags `apple`/`android` для автоматического выбора:
  ```rust
  #[cfg(any(feature = "apple", feature = "android"))]
  #[default]
  Normal,
  #[cfg(not(any(feature = "apple", feature = "android")))]
  #[default]
  High,
  ```
- Документировать рекомендуемые пресеты для платформ в README

---

### 2.3 [ВЫСОКИЙ] Thundering herd в HLS `wait_range`

**Файл:** `kithara-hls/src/source.rs:996`

```rust
self.shared.condvar.wait(&mut segments);
```

Единственный `Condvar` будит **всех** ожидающих при загрузке каждого сегмента. В типичном сценарии одновременного чтения и seek-а это вызывает ненужные пробуждения.

**Проблема для мобильных:** Каждое ложное пробуждение = переключение контекста = расход энергии батареи.

**Рекомендации:**
- Использовать `notify_one()` вместо `notify_all()` где возможно (если есть один reader)
- Рассмотреть `tokio::sync::Notify` с per-reader ожиданием вместо общего `Condvar`
- Добавить backoff при ожидании: вместо немедленного re-lock после `condvar.wait`, проверять lock-free очередь сначала (как в `StorageResource`)

---

### 2.4 [ВЫСОКИЙ] Синхронное вытеснение из кэша (eviction) при открытии ресурса

**Файл:** `kithara-assets/src/evict.rs`

Eviction запускается синхронно при первом `open_resource()` для нового `asset_root`. Итерация по файловой системе + удаление файлов блокируют вызывающий поток.

**Проблема для мобильных:** NAND-операции на мобильных медленнее, особенно при записи. Блокирующая I/O на пути воспроизведения = задержка первого звука.

**Рекомендации:**
- Вынести eviction в отдельный `tokio::spawn` (фоновая задача)
- Не блокировать воспроизведение ожиданием eviction
- Добавить lazy eviction: отложить удаление до idle-состояния

---

### 2.5 [ВЫСОКИЙ] Размер mmap при потоковом воспроизведении

**Файл:** `kithara-storage/src/resource.rs`

`StorageResource` использует `mmap-io` для случайного доступа. На мобильных устройствах:
- Virtual memory mapping имеет более жёсткие лимиты
- Kernel может агрессивнее отзывать mapped pages при memory pressure
- Большие mmap-регионы могут вызвать page fault storms

**Рекомендации:**
- Ограничить размер mmap-региона для мобильных (например, 32 МБ sliding window)
- Использовать `madvise(MADV_SEQUENTIAL)` для потоковых чтений
- Использовать `madvise(MADV_DONTNEED)` для уже прочитанных регионов
- Рассмотреть fallback на `pread`/`pwrite` для маленьких файлов (< 1 МБ) вместо mmap

---

### 2.6 [СРЕДНИЙ] Нет ограничения на количество одновременных HTTP-соединений

**Файл:** `kithara-net/src/client.rs`

`reqwest::Client` по умолчанию использует пул соединений без жёсткого лимита. На мобильных:
- Каждое TCP-соединение потребляет FD и память
- При переключении WiFi ↔ cellular все соединения инвалидируются
- DNS-разрешение может быть медленным

**Рекомендации:**
- Установить `pool_max_idle_per_host(2)` для мобильных
- Добавить `tcp_keepalive` для раннего обнаружения разрыва
- Рассмотреть обработку network change events (смена сети → reset client)

---

### 2.7 [СРЕДНИЙ] Нет адаптации ABR к мобильному контексту

**Файл:** `kithara-abr/src/controller.rs`

ABR-контроллер оперирует throughput и buffer level, но не учитывает:
- Тип сети (WiFi vs cellular vs offline)
- Уровень заряда батареи
- Thermal state устройства

**Рекомендации:**
- Добавить `AbrHints` структуру с mobile-контекстом:
  ```rust
  pub struct AbrHints {
      pub network_type: Option<NetworkType>,
      pub battery_level: Option<f32>,
      pub thermal_state: Option<ThermalState>,
  }
  ```
- При низком заряде / thermal throttling — автоматически понижать вариант
- При cellular — более консервативная стратегия (больший buffer target)

---

### 2.8 [СРЕДНИЙ] Отсутствие partial segment resume

HLS-downloader при сбое сети перезагружает сегмент с начала. На мобильных с нестабильным соединением это означает потерю уже загруженных данных.

**Рекомендация:** Использовать HTTP Range requests для возобновления загрузки сегмента с последнего успешного байта. `StorageResource` уже поддерживает `write_at(offset)`, поэтому архитектура позволяет это реализовать.

---

### 2.9 [СРЕДНИЙ] `spawn_blocking` для декодирования вместо выделенного потока

**Файл:** `kithara-audio/src/pipeline/audio.rs:19`

```rust
use tokio::task::spawn_blocking;
```

`spawn_blocking` использует пул потоков tokio, который делится с другими задачами. При высокой нагрузке декодер может быть вытеснен, что приведёт к underrun.

**Текущая реализация** (в `worker.rs`) правильно использует выделенный OS-поток, но инициализация через `spawn_blocking` может конкурировать с другими blocking-задачами.

**Рекомендация:** Убедиться, что весь decode path выполняется на выделенном потоке с приоритетом, а не через `spawn_blocking`. На Android/iOS стоит установить thread priority (audio thread).

---

### 2.10 [НИЗКИЙ] Нет graceful degradation при low memory

При получении memory warning от ОС нет механизма для:
- Сброса буферных пулов
- Уменьшения размера LRU-кэша
- Прекращения prefetch-а следующих сегментов

**Рекомендация:** Добавить callback/hook для memory pressure:
```rust
pub trait MemoryPressureHandler {
    fn on_memory_warning(&self, level: MemoryPressureLevel);
}
```

---

## Часть 3: Сводная таблица рекомендаций

| # | Категория | Приоритет | Описание | Усилия |
|---|-----------|-----------|----------|--------|
| 2.1 | Mobile | Критический | Конфигурируемые размеры пулов | Низкие |
| 2.2 | Mobile | Критический | Адаптивное качество ресемплера | Низкие |
| 1.1 | Maintain | Высокий | PcmPool в декодере | Средние |
| 1.2 | Maintain | Высокий | Не держать лок при I/O в кэше | Средние |
| 2.3 | Mobile | Высокий | Thundering herd в HLS condvar | Средние |
| 2.4 | Mobile | Высокий | Асинхронный eviction | Средние |
| 2.5 | Mobile | Высокий | Ограничение mmap на mobile | Высокие |
| 1.3 | Maintain | Средний | Дебаунс персистенции пинов | Средние |
| 1.4 | Maintain | Средний | Конфигурируемый LRU-кэш | Низкие |
| 1.5 | Maintain | Средний | Единый WaitableRange | Высокие |
| 1.6 | Maintain | Средний | Убрать expect() в prod | Низкие |
| 2.6 | Mobile | Средний | Лимит HTTP-соединений | Низкие |
| 2.7 | Mobile | Средний | Mobile-aware ABR | Высокие |
| 2.8 | Mobile | Средний | Partial segment resume | Средние |
| 2.9 | Mobile | Средний | Приоритет decode-потока | Низкие |
| 1.7 | Maintain | Низкий | Workspace-level perf feature | Низкие |
| 1.8 | Maintain | Низкий | CI для fmt/clippy | Низкие |
| 2.10 | Mobile | Низкий | Memory pressure hook | Средние |

---

## Часть 4: Quick Wins (можно реализовать за один PR)

1. **Конфигурируемые пулы** — добавить параметры в `AudioConfig` для override размеров `BytePool`/`PcmPool`
2. **Дефолт ресемплера** — `#[cfg]`-based дефолт `Normal` для mobile features
3. **LRU capacity** — параметр в `AssetStoreBuilder`
4. **HTTP pool limits** — `pool_max_idle_per_host(2)` + `tcp_keepalive`
5. **CI pipeline** — добавить `ci.yml` с fmt/clippy/test/doc
6. **`expect()` → `const`** — замена в `cache.rs:62`
