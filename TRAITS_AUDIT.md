# Полный аудит pub traits для интеграции Mockall

## Обзор

Проведен полный аудит всех публичных traits во всех 9 крейтах проекта Kithara.
Оценена совместимость каждого trait с mockall и определены приоритеты.

**Дата аудита:** 2026-01-20
**Версия mockall:** 0.13

---

## Критерии оценки

### ✅ Совместим с mockall (рекомендуется)
- Простые sync traits без associated types
- Async traits без сложных associated types (mockall поддерживает #[async_trait])
- Traits с простыми generic параметрами

### ⚠️ Требует оценки
- Traits с associated types (зависит от сложности)
- Marker traits (пустые) - возможно не нужен mock
- Extension traits с default impl - возможно не нужен mock

### ❌ Несовместим / не рекомендуется
- Traits со сложными associated types (требуют concrete types для mock)
- Traits с lifetime параметрами в return types
- Traits с Pin<Box<dyn Trait>> (сложно мокировать)

---

## Детальный аудит по крейтам

### 1. kithara-storage

#### `Resource` (resource.rs:27)
```rust
pub trait Resource: Send + Sync + 'static {
    async fn write(&self, data: &[u8]) -> StorageResult<()>;
    async fn read(&self) -> StorageResult<Bytes>;
}
```
- **Статус:** ✅ Совместим
- **Тип:** Async trait (простой)
- **Приоритет:** СРЕДНИЙ
- **Применение:** Изоляция storage layer в тестах
- **Рекомендация:** Добавить `#[cfg_attr(test, automock)]`

#### `StreamingResourceExt` (resource.rs:57)
```rust
pub trait StreamingResourceExt: Resource {
    async fn wait_range(...) -> StorageResult<WaitOutcome>;
    async fn read_at(...) -> StorageResult<Bytes>;
    async fn write_at(...) -> StorageResult<()>;
    // ... другие методы
}
```
- **Статус:** ✅ Совместим
- **Тип:** Async trait (extends Resource)
- **Приоритет:** НИЗКИЙ
- **Применение:** Редко используется напрямую
- **Рекомендация:** Можно добавить automock позже

#### `AtomicResourceExt` (resource.rs:82)
```rust
pub trait AtomicResourceExt: Resource {}
```
- **Статус:** ⚠️ Marker trait
- **Тип:** Пустой marker trait
- **Приоритет:** НЕ НУЖЕН
- **Рекомендация:** Не мокировать (пустой)

---

### 2. kithara-file

**Pub traits:** Нет

---

### 3. kithara-net

#### `Net` (traits.rs:18) ⭐ КЛЮЧЕВОЙ
```rust
pub trait Net: Send + Sync {
    async fn get_bytes(&self, url: Url, headers: Option<Headers>) -> Result<Bytes, NetError>;
    async fn stream(&self, url: Url, headers: Option<Headers>) -> Result<ByteStream, NetError>;
    async fn get_range(&self, url: Url, range: RangeSpec, headers: Option<Headers>) -> Result<ByteStream, NetError>;
    async fn head(&self, url: Url, headers: Option<Headers>) -> Result<Headers, NetError>;
}
```
- **Статус:** ✅ Совместим
- **Тип:** Async trait (4 метода)
- **Приоритет:** ВЫСОКИЙ ⭐
- **Применение:** Критичен для изоляции network layer в HLS/File тестах
- **Рекомендация:** ОБЯЗАТЕЛЬНО добавить `#[cfg_attr(test, automock)]`
- **Примечание:** Уже использует #[async_trait], mockall поддерживает

#### `NetExt` (traits.rs:40)
```rust
pub trait NetExt: Net + Sized {
    fn with_timeout(self, timeout: Duration) -> TimeoutNet<Self> { ... }
    fn with_retry(self, policy: RetryPolicy) -> RetryNet<Self, DefaultRetryPolicy> { ... }
}
```
- **Статус:** ⚠️ Extension trait
- **Тип:** Extension trait с default impl
- **Приоритет:** НЕ НУЖЕН
- **Рекомендация:** Не мокировать (extension methods)

#### `RetryClassifier` (retry.rs:15)
```rust
pub trait RetryClassifier {
    fn should_retry(&self, error: &NetError) -> bool;
}
```
- **Статус:** ✅ Совместим
- **Тип:** Простой sync trait
- **Приоритет:** НИЗКИЙ
- **Применение:** Тестирование retry logic
- **Рекомендация:** Можно добавить automock

#### `RetryPolicyTrait` (retry.rs:199)
```rust
pub trait RetryPolicyTrait: Send + Sync {
    fn should_retry(&self, error: &NetError, attempt: u32) -> bool;
    fn delay_for_attempt(&self, attempt: u32) -> Duration;
    fn max_attempts(&self) -> u32;
}
```
- **Статус:** ✅ Совместим
- **Тип:** Простой sync trait
- **Приоритет:** НИЗКИЙ
- **Применение:** Тестирование retry policies
- **Рекомендация:** Можно добавить automock

---

### 4. kithara-assets

#### `Assets` (base.rs:44)
```rust
pub trait Assets: Clone + Send + Sync + 'static {
    type StreamingRes: Clone + Send + Sync + 'static;
    type AtomicRes: Clone + Send + Sync + 'static;

    async fn open_atomic_resource(&self, key: &ResourceKey) -> AssetsResult<Self::AtomicRes>;
    async fn open_streaming_resource(&self, key: &ResourceKey) -> AssetsResult<Self::StreamingRes>;
    // ... другие методы
}
```
- **Статус:** ❌ Несовместим (сложные associated types)
- **Тип:** Async trait с 2 associated types
- **Приоритет:** MANUAL MOCK
- **Применение:** Критичный trait, но слишком сложный для mockall
- **Рекомендация:** ОСТАВИТЬ ручной mock (уже есть TestAssets в fixture)
- **Причина:** Associated types требуют concrete implementations для mock

---

### 5. kithara-bufpool

#### `Reuse` (lib.rs:48)
```rust
pub trait Reuse {
    fn reuse(&mut self, trim_to_size: Option<usize>);
}
```
- **Статус:** ✅ Совместим
- **Тип:** Простой sync trait
- **Приоритет:** НИЗКИЙ
- **Применение:** Внутренняя логика буферного пула
- **Рекомендация:** Можно добавить automock для полноты

---

### 6. kithara-stream

#### `Source` (source.rs:38) ⭐ ВАЖНЫЙ
```rust
pub trait Source: Send + Sync + 'static {
    type Item: Send + 'static;
    type Error: std::error::Error + Send + Sync + 'static;

    async fn read_at(&self, offset: u64, buf: &mut [Self::Item]) -> Result<usize, Self::Error>;
    async fn size(&self) -> Result<Option<u64>, Self::Error>;
    fn handle(&self) -> SourceHandle;
}
```
- **Статус:** ⚠️ Требует оценки (associated types)
- **Тип:** Async trait с 2 associated types (Item, Error)
- **Приоритет:** СРЕДНИЙ
- **Применение:** Ключевой trait для stream orchestration
- **Рекомендация:** Попробовать mockall с concrete types (Item=u8, Error=Box<dyn Error>)
- **Альтернатива:** Manual mock если mockall не справится

#### `SourceFactory` (facade.rs:35)
```rust
pub trait SourceFactory: Send + Sync + 'static {
    type Params: Send + Default;
    type Event: Clone + Send + 'static;
    type SourceImpl: Source<Item = u8> + Send + Sync;

    async fn open(url: Url, params: Self::Params) -> Result<Opened<Self>, Self::Error>;
}
```
- **Статус:** ❌ Сложный (3 associated types + bounded)
- **Тип:** Async trait с 3 associated types
- **Приоритет:** MANUAL MOCK
- **Применение:** Factory pattern для создания sources
- **Рекомендация:** Оставить manual mock (слишком сложный)

#### `PrefetchSource` (prefetch.rs:27)
```rust
pub trait PrefetchSource: AsyncWorkerSource {}
```
- **Статус:** ⚠️ Marker trait
- **Тип:** Пустой marker trait (blanket impl)
- **Приоритет:** НЕ НУЖЕН
- **Рекомендация:** Не мокировать (blanket impl для всех AsyncWorkerSource)

#### `BlockingSource` (prefetch.rs:30)
```rust
pub trait BlockingSource: SyncWorkerSource {}
```
- **Статус:** ⚠️ Marker trait
- **Тип:** Пустой marker trait (blanket impl)
- **Приоритет:** НЕ НУЖЕН
- **Рекомендация:** Не мокировать (blanket impl для всех SyncWorkerSource)

---

### 7. kithara-worker

#### `WorkerItem` (lib.rs:32)
```rust
pub trait WorkerItem: Send + 'static {
    type Data: Send + 'static;

    fn data(&self) -> &Self::Data;
}
```
- **Статус:** ⚠️ Associated type
- **Тип:** Sync trait с 1 associated type
- **Приоритет:** НИЗКИЙ
- **Применение:** Внутренняя worker логика
- **Рекомендация:** Оценить необходимость (возможно не нужен mock)

#### `ItemValidator` (lib.rs:109)
```rust
pub trait ItemValidator<I: WorkerItem> {
    fn is_valid(&self, item: &I) -> bool;
}
```
- **Статус:** ✅ Совместим (generic)
- **Тип:** Простой sync trait с generic
- **Приоритет:** НИЗКИЙ
- **Применение:** Валидация items в worker
- **Рекомендация:** Можно добавить automock

#### `AsyncWorkerSource` (lib.rs:192)
```rust
pub trait AsyncWorkerSource: Send + 'static {
    type Chunk: Send + 'static;
    type Command: Send + 'static;

    async fn pull(&mut self, cmd: &mut Receiver<Self::Command>) -> WorkerResult<Option<Self::Chunk>>;
}
```
- **Статус:** ⚠️ Внутренний trait
- **Тип:** Async trait с 2 associated types
- **Приоритет:** НИЗКИЙ
- **Применение:** Внутренняя worker abstraction
- **Рекомендация:** Не нужен mock (используется через PrefetchSource)

#### `SyncWorkerSource` (lib.rs:355)
```rust
pub trait SyncWorkerSource: Send + 'static {
    type Chunk: Send + 'static;
    type Command: Send + 'static;

    fn pull_blocking(&mut self, cmd: &mut Receiver<Self::Command>) -> WorkerResult<Option<Self::Chunk>>;
}
```
- **Статус:** ⚠️ Внутренний trait
- **Тип:** Sync trait с 2 associated types
- **Приоритет:** НИЗКИЙ
- **Применение:** Внутренняя worker abstraction
- **Рекомендация:** Не нужен mock (используется через BlockingSource)

---

### 8. kithara-decode

#### `Decoder` (decoder.rs:12) ⭐ ВАЖНЫЙ
```rust
pub trait Decoder: Send + 'static {
    fn next_chunk(&mut self) -> DecodeResult<Option<PcmChunk<f32>>>;
    fn spec(&self) -> PcmSpec;
}
```
- **Статус:** ✅ Совместим
- **Тип:** Простой sync trait (2 метода)
- **Приоритет:** СРЕДНИЙ
- **Применение:** Изоляция decoding logic в Pipeline тестах
- **Рекомендация:** Добавить `#[cfg_attr(test, automock)]`
- **Примечание:** Уже есть хороший ручной MockDecoder (207 строк), можно заменить

---

### 9. kithara-hls

#### `Estimator` (abr/estimator.rs:6) ⭐ КЛЮЧЕВОЙ
```rust
pub trait Estimator {
    fn estimate_bps(&self) -> Option<u64>;
    fn push_sample(&mut self, sample: ThroughputSample);
}
```
- **Статус:** ✅ Совместим
- **Тип:** Простой sync trait (2 метода)
- **Приоритет:** ВЫСОКИЙ ⭐
- **Применение:** Изоляция throughput estimation в ABR тестах
- **Рекомендация:** ОБЯЗАТЕЛЬНО добавить `#[cfg_attr(test, automock)]`
- **Примечание:** Уже есть MockEstimator в тестах (15 строк), легко заменить

---

## Приоритизация для Phase 1.2

### Высокий приоритет (начать немедленно) ⭐

1. **`kithara_net::Net`** - критичен для изоляции network layer
   - Файл: `crates/kithara-net/src/traits.rs:18`
   - Действие: Добавить `#[cfg_attr(test, automock)]` ПЕРЕД `#[async_trait]`

2. **`kithara_hls::abr::Estimator`** - критичен для ABR тестов
   - Файл: `crates/kithara-hls/src/abr/estimator.rs:6`
   - Действие: Добавить `#[cfg_attr(test, automock)]`

### Средний приоритет (после базовой интеграции)

3. **`kithara_decode::Decoder`** - изоляция decoding
   - Файл: `crates/kithara-decode/src/decoder.rs:12`
   - Действие: Добавить `#[cfg_attr(test, automock)]`

4. **`kithara_stream::Source`** - попробовать с concrete types
   - Файл: `crates/kithara-stream/src/source.rs:38`
   - Действие: Оценить возможность mockall с `Item=u8, Error=Box<dyn Error>`

5. **`kithara_storage::Resource`** - изоляция storage
   - Файл: `crates/kithara-storage/src/resource.rs:27`
   - Действие: Добавить `#[cfg_attr(test, automock)]`

### Низкий приоритет (опционально)

- `kithara_net::RetryClassifier`
- `kithara_net::RetryPolicyTrait`
- `kithara_bufpool::Reuse`
- `kithara_worker::ItemValidator`

### Не мокировать

- `kithara_assets::Assets` - ❌ слишком сложный (manual mock)
- `kithara_stream::SourceFactory` - ❌ слишком сложный (manual mock)
- Extension traits (NetExt, AtomicResourceExt)
- Marker traits (PrefetchSource, BlockingSource)
- Внутренние worker traits (AsyncWorkerSource, SyncWorkerSource)

---

## Статистика

### Всего pub traits: 18

**По совместимости:**
- ✅ Совместим с mockall: 9 traits (50%)
- ⚠️ Требует оценки: 6 traits (33%)
- ❌ Несовместим: 3 traits (17%)

**По приоритету:**
- ⭐ Высокий приоритет: 2 traits (Net, Estimator)
- Средний приоритет: 3 traits (Decoder, Source, Resource)
- Низкий приоритет: 4 traits
- Не нужен mock: 9 traits

**По типу:**
- Async traits: 7
- Sync traits: 11
- С associated types: 7
- Без associated types: 11

---

## Рекомендации по реализации

### Phase 1.2: Подготовка traits

1. **Сначала:** Добавить automock к высокоприоритетным traits (Net, Estimator)
2. **Потом:** Добавить к среднеприоритетным (Decoder, Resource)
3. **Опционально:** Попробовать Source с concrete types
4. **Не трогать:** Assets, SourceFactory (оставить manual mocks)

### Порядок изменений

```bash
# Приоритет 1
crates/kithara-net/src/traits.rs          # Net trait
crates/kithara-hls/src/abr/estimator.rs   # Estimator trait

# Приоритет 2
crates/kithara-decode/src/decoder.rs      # Decoder trait
crates/kithara-storage/src/resource.rs    # Resource trait

# Приоритет 3 (оценить)
crates/kithara-stream/src/source.rs       # Source trait (с осторожностью)
```

### Паттерн добавления

**Для async traits:**
```rust
use mockall::automock;

#[cfg_attr(test, automock)]
#[async_trait]
pub trait Net: Send + Sync {
    // ...
}
```

**Для sync traits:**
```rust
use mockall::automock;

#[cfg_attr(test, automock)]
pub trait Estimator {
    // ...
}
```

---

## Ожидаемые результаты

После Phase 1.2:
- ✅ 2 высокоприоритетных traits готовы к использованию (Net, Estimator)
- ✅ Mockall генерирует MockNet и MockEstimator автоматически
- ✅ Готовы к рефакторингу тестов в Phase 1.3

После Phase 2:
- ✅ 3 среднеприоритетных traits добавлены (Decoder, Resource, возможно Source)
- ✅ Большинство ключевых abstractions изолированы

---

## Критерии успеха

1. ✅ Все высокоприоритетные traits имеют `#[cfg_attr(test, automock)]`
2. ✅ `cargo check --workspace` проходит успешно
3. ✅ `cargo test --workspace` проходит успешно (моки пока не используются)
4. ✅ Public API не изменился (automock только в test builds)
5. ✅ Нет конфликтов с существующими ручными моками
