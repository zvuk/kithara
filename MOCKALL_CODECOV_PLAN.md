# План интеграции Mockall + Codecov для проекта Kithara

## Обзор

Интеграция professional-grade mocking framework (mockall) и автоматизированного code coverage (codecov) для улучшения качества тестов и метрик покрытия.

**Цели:**
- Заменить ручные моки на mockall для лучшей выразительности и maintainability
- Добавить автоматизированный code coverage reporting
- Улучшить изоляцию unit-тестов
- Увеличить coverage с текущего уровня до 80%+
- Добавить property-based testing для критических алгоритмов

---

## Фаза 0: Анализ и подготовка

### 0.1 Аудит текущих моков и трейтов

**ВАЖНО:** Аудит проводится для ВСЕХ 9 крейтов проекта без исключения.

**Существующие ручные моки (для замены):**
- `tests/kithara_decode/mock_decoder.rs` - MockDecoder (207 строк)
- `tests/kithara_decode/pipeline_unit_test.rs` - SimpleMockDecoder (30 строк)
- `crates/kithara-hls/src/abr/controller.rs` - MockEstimator (15 строк в тестах)
- Различные test fixtures в `tests/tests/kithara_hls/fixture/`

**Ключевые traits для мокирования (по крейтам):**

#### kithara-net
1. **`Net`** (async trait) - 4 метода
   - `get_bytes()`, `stream()`, `get_range()`, `head()`
   - Используется в: FetchManager, HttpClient tests
   - Приоритет: ВЫСОКИЙ (критичный для HLS/network isolation)

#### kithara-decode
2. **`Decoder`** - 2 метода
   - `next_chunk()`, `spec()`
   - Используется в: Pipeline, integration tests
   - Приоритет: СРЕДНИЙ (уже есть хорошие ручные моки)

#### kithara-hls
3. **`abr::Estimator`** - 2 метода
   - `estimate_bps()`, `push_sample()`
   - Используется в: AbrController tests
   - Приоритет: ВЫСОКИЙ (нужна изоляция от throughput calculation)

#### kithara-assets
4. **`Assets`** - 3+ методов (associated types)
   - `open_atomic_resource()`, `open_streaming_resource()`, etc.
   - Используется в: FetchManager, integration tests
   - Приоритет: НИЗКИЙ (сложные associated types, оставить manual mock)

#### kithara-storage
5. **`StreamingResource`** - seek/read/write
   - Приоритет: НИЗКИЙ (специфичная логика, manual mock лучше)
6. **`AtomicResource`** - read/replace
   - Приоритет: НИЗКИЙ

#### kithara-stream
7. **`Source`** - async trait для byte streams
   - `read_at()`, `size()`, `handle()`
   - Приоритет: СРЕДНИЙ (может быть полезен для изоляции)

#### kithara-worker
8. **Внутренние traits** (если есть)
   - Требует аудита исходников
   - Приоритет: НИЗКИЙ (скорее всего нет публичных traits)

#### kithara-file, kithara-bufpool
9-10. **Traits для аудита**
   - Требует детального анализа
   - Приоритет: определится после аудита

**Анализ зависимостей тестов:**
```
Unit tests (src/):
  - 241 unit-тестов
  - Используют простые моки (MockEstimator)
  - Хорошие кандидаты для mockall

Integration tests (tests/):
  - 360 интеграционных тестов
  - Используют fixtures (TestServer, TestAssets)
  - Частично подходят для mockall (изолировать network layer)
```

**Полный аудит traits (TODO для Phase 0):**
1. **kithara-storage** - найти все pub traits, оценить complexity
2. **kithara-file** - найти все pub traits, проверить async
3. **kithara-net** - ✓ уже проанализирован (Net trait)
4. **kithara-assets** - ✓ уже проанализирован (Assets trait)
5. **kithara-bufpool** - найти все pub traits
6. **kithara-stream** - ✓ частично проанализирован (Source trait)
7. **kithara-worker** - найти внутренние traits
8. **kithara-decode** - ✓ уже проанализирован (Decoder trait)
9. **kithara-hls** - ✓ уже проанализирован (Estimator trait)

**Критерии для mockall vs manual mock:**
- ✅ Mockall: простые sync/async traits без associated types
- ✅ Mockall: traits с generic методами (если не слишком сложные)
- ❌ Manual: traits со сложными associated types (как Assets)
- ❌ Manual: traits с lifetime параметрами в return types
- 🤔 Evaluate: traits с Pin<Box<dyn Stream>> (можно через returning)

### 0.2 Измерение baseline coverage

**Действия:**
1. Установить cargo-tarpaulin локально
2. Запустить baseline coverage report
3. Определить модули с низким покрытием
4. Создать coverage badge для README

**Ожидаемые проблемы:**
- tarpaulin имеет ограниченную поддержку macOS (использовать Linux/Docker)
- Async код может показывать неточное покрытие
- Integration тесты могут искажать метрики

---

## Фаза 1: Интеграция Mockall (базовая)

### 1.1 Добавление зависимостей

**`Cargo.toml` (workspace root):**
```toml
[workspace.dependencies]
mockall = "0.13"  # Latest stable
```

**Все крейты для обновления (9 крейтов):**
- `kithara-storage/Cargo.toml` - добавить mockall в dev-dependencies
- `kithara-file/Cargo.toml` - добавить mockall в dev-dependencies
- `kithara-net/Cargo.toml` - добавить mockall в dev-dependencies
- `kithara-assets/Cargo.toml` - добавить mockall в dev-dependencies
- `kithara-bufpool/Cargo.toml` - добавить mockall в dev-dependencies
- `kithara-stream/Cargo.toml` - добавить mockall в dev-dependencies
- `kithara-worker/Cargo.toml` - добавить mockall в dev-dependencies
- `kithara-decode/Cargo.toml` - добавить mockall в dev-dependencies
- `kithara-hls/Cargo.toml` - добавить mockall в dev-dependencies

**Обоснование:** Mockall добавляется во ВСЕ крейты без исключения для:
- Единообразия тестовой инфраструктуры
- Возможности мокировать любые зависимости в будущем
- Подготовки к дальнейшему рефакторингу тестов

### 1.2 Подготовка traits для automocking

**КРИТИЧНО:** Анализ и подготовка traits проводится для ВСЕХ крейтов!

**Приоритет 1 (начать с этих):**

1. **`kithara-net::Net` - async trait**
   - Уже использует `#[async_trait]`
   - Mockall поддерживает async_trait напрямую
   - Добавить `#[cfg_attr(test, automock)]` ПЕРЕД `#[async_trait]`

2. **`kithara-hls::abr::Estimator`**
   - Чистый sync trait
   - Простое добавление `#[cfg_attr(test, automock)]`

3. **`kithara-decode::Decoder`**
   - Sync trait с generic return type
   - Может потребовать `#[concretize]` для Option<PcmChunk>

**Приоритет 2 (после базовой интеграции):**

4. **`kithara-stream::Source`** - async trait
   - Проверить совместимость с mockall
   - Добавить automock если подходит

5. **Другие pub traits** в остальных крейтах
   - kithara-storage: StreamingResource, AtomicResource
   - kithara-file: проверить наличие pub traits
   - kithara-bufpool: проверить наличие pub traits
   - kithara-worker: проверить внутренние traits
   - kithara-assets: оставить manual mock (сложные associated types)

**Пример изменений:**

```rust
// crates/kithara-net/src/traits.rs
use mockall::automock;

#[cfg_attr(test, automock)]  // НОВОЕ: только для test builds
#[async_trait]
pub trait Net: Send + Sync {
    async fn get_bytes(&self, url: Url, headers: Option<Headers>) -> Result<Bytes, NetError>;
    // ... остальные методы
}
```

```rust
// crates/kithara-hls/src/abr/estimator.rs
use mockall::automock;

#[cfg_attr(test, automock)]
pub trait Estimator {
    fn estimate_bps(&self) -> Option<u64>;
    fn push_sample(&mut self, sample: ThroughputSample);
}
```

### 1.3 Рефакторинг первых unit-тестов

**Целевой файл:** `crates/kithara-hls/src/abr/controller.rs`

**Было (ручной mock):**
```rust
#[derive(Clone)]
struct MockEstimator {
    estimate: Option<u64>,
    call_count: Arc<AtomicUsize>,
}

impl Estimator for MockEstimator {
    fn estimate_bps(&self) -> Option<u64> {
        self.call_count.fetch_add(1, Ordering::SeqCst);
        self.estimate
    }
    fn push_sample(&mut self, _sample: ThroughputSample) {}
}
```

**Станет (mockall):**
```rust
use super::MockEstimator;  // Auto-generated by #[automock]

#[test]
fn test_estimator_called_once_per_decide() {
    let mut mock_estimator = MockEstimator::new();

    mock_estimator
        .expect_estimate_bps()
        .times(1)  // Built-in call count verification!
        .returning(|| Some(1_000_000));

    let c = AbrController::with_estimator(cfg, mock_estimator, None);
    c.decide(&variants(), 5.0, now);

    // Mock automatically verifies call count on drop
}
```

**Преимущества:**
- Встроенная верификация вызовов (убирает Arc<AtomicUsize>)
- Более выразительный API
- Автоматическая проверка при drop (не нужны явные assert)
- Поддержка sequences для проверки порядка

### 1.4 Измерение улучшений

**Метрики до/после:**
- Количество строк mock кода: 207 → ~50 (ожидается)
- Выразительность тестов: субъективно улучшится
- Скорость выполнения: должна остаться прежней
- Количество ложных срабатываний: уменьшится (строгая верификация)

---

## Фаза 2: Расширенное использование Mockall

### 2.1 Изоляция network layer в HLS тестах

**Проблема:** Integration тесты создают реальный HTTP server (TestServer)
**Решение:** Использовать MockNet для изоляции

**Целевые файлы:**
- `tests/tests/kithara_hls/basic_playback.rs`
- `tests/tests/kithara_hls/abr_integration.rs`
- `tests/tests/kithara_hls/keys_integration.rs`

**Пример рефакторинга:**

```rust
// Было: реальный HTTP server
#[rstest]
#[tokio::test]
async fn test_basic_playback(assets_fixture: TestAssets, net_fixture: HttpClient) {
    let server = TestServer::new().await;  // Реальный HTTP сервер!
    let url = server.url("/master.m3u8").unwrap();
    // ...
}

// Станет: mock network
#[rstest]
#[tokio::test]
async fn test_basic_playback_isolated(assets_fixture: TestAssets) {
    use kithara_net::MockNet;

    let mut mock_net = MockNet::new();

    // Setup expectations для master playlist
    mock_net
        .expect_get_bytes()
        .withf(|url, _| url.path() == "/master.m3u8")
        .times(1)
        .returning(|_, _| Ok(Bytes::from(MASTER_PLAYLIST_CONTENT)));

    // Setup expectations для media playlists
    mock_net
        .expect_get_bytes()
        .withf(|url, _| url.path() == "/v0.m3u8")
        .returning(|_, _| Ok(Bytes::from(MEDIA_PLAYLIST_CONTENT)));

    // Тест использует mock вместо реального HTTP
    let hls = Hls::open_with_net(url, params, mock_net).await.unwrap();
    // ...
}
```

**Преимущества:**
- Нет зависимости от network stack
- Тесты выполняются быстрее (нет HTTP overhead)
- Детерминистичное поведение
- Легко тестировать ошибки (timeouts, 404, etc.)

### 2.2 Sequence testing для ABR

**Проблема:** ABR controller должен вызывать estimator в правильном порядке

```rust
use mockall::Sequence;

#[test]
fn test_abr_sequence_verification() {
    let mut seq = Sequence::new();
    let mut mock_estimator = MockEstimator::new();

    // Проверяем порядок: estimate → push_sample → estimate
    mock_estimator
        .expect_estimate_bps()
        .times(1)
        .in_sequence(&mut seq)
        .returning(|| Some(1_000_000));

    mock_estimator
        .expect_push_sample()
        .times(1)
        .in_sequence(&mut seq)
        .return_const(());

    mock_estimator
        .expect_estimate_bps()
        .times(1)
        .in_sequence(&mut seq)
        .returning(|| Some(2_000_000));

    // Тест
    let controller = AbrController::with_estimator(cfg, mock_estimator, None);
    controller.decide(...);
    controller.record_throughput(...);
    controller.decide(...);

    // Mockall автоматически проверит порядок вызовов
}
```

### 2.3 Matcher patterns для сложных аргументов

**Использование predicate matchers:**

```rust
use mockall::predicate::*;

#[test]
fn test_net_range_requests() {
    let mut mock_net = MockNet::new();

    // Matcher для проверки range requests
    mock_net
        .expect_get_range()
        .withf(|url, range, _headers| {
            url.path().contains("/segment") &&
            range.start() == 0 &&
            range.end() == Some(1024)
        })
        .returning(|_, _, _| {
            // Return mock stream
            Ok(Box::pin(futures::stream::once(async {
                Ok(Bytes::from(vec![0; 1024]))
            })))
        });
}
```

---

## Фаза 3: Codecov интеграция

### 3.1 Локальная настройка coverage

**Установка tarpaulin (Linux/Docker):**

```bash
# Локально (Linux)
cargo install cargo-tarpaulin

# Или через Docker
docker run --security-opt seccomp=unconfined \
  -v "${PWD}:/volume" \
  xd009642/tarpaulin:latest \
  cargo tarpaulin --out Xml --output-dir ./coverage
```

**Конфигурация: `tarpaulin.toml`**

```toml
[report]
# Форматы отчетов
out = ["Html", "Xml", "Lcov"]

[run]
# Исключить файлы
exclude = [
    "*/tests/*",
    "*/examples/*",
    "*/benches/*"
]

# Timeout для медленных тестов
timeout = "5m"

# Запускать доктесты
run-types = ["Tests", "Doctests"]

# Количество параллельных тестов
test-threads = 4

[report.html]
output-dir = "target/coverage/html"

[report.xml]
output-dir = "target/coverage"
```

### 3.2 GitHub Actions workflow

**Файл: `.github/workflows/coverage.yml`**

```yaml
name: Code Coverage

on:
  push:
    branches: [main]
  pull_request:
    branches: [main]

jobs:
  coverage:
    runs-on: ubuntu-latest

    steps:
      - uses: actions/checkout@v4

      - name: Install Rust
        uses: dtolnay/rust-toolchain@stable

      - name: Install tarpaulin
        run: cargo install cargo-tarpaulin

      - name: Generate coverage
        run: |
          cargo tarpaulin \
            --workspace \
            --timeout 300 \
            --out Xml \
            --output-dir ./coverage

      - name: Upload to codecov
        uses: codecov/codecov-action@v4
        with:
          token: ${{ secrets.CODECOV_TOKEN }}
          files: ./coverage/cobertura.xml
          fail_ci_if_error: true
          verbose: true

      - name: Archive coverage results
        uses: actions/upload-artifact@v4
        with:
          name: coverage-report
          path: coverage/
```

### 3.3 Codecov конфигурация

**Файл: `codecov.yml`**

```yaml
coverage:
  status:
    project:
      default:
        target: 80%  # Минимальный coverage
        threshold: 2%  # Допустимое снижение

    patch:
      default:
        target: 70%  # Coverage для новых изменений
        threshold: 5%

ignore:
  - "tests/**/*"
  - "examples/**/*"
  - "benches/**/*"

comment:
  layout: "reach, diff, flags, files"
  behavior: default
  require_changes: false

flags:
  unit:
    paths:
      - src/
  integration:
    paths:
      - tests/
```

### 3.4 Coverage badges

**README.md обновления:**

```markdown
# Kithara

[![codecov](https://codecov.io/gh/YOUR_ORG/kithara/branch/main/graph/badge.svg)](https://codecov.io/gh/YOUR_ORG/kithara)
[![CI](https://github.com/YOUR_ORG/kithara/workflows/CI/badge.svg)](https://github.com/YOUR_ORG/kithara/actions)

Audio streaming library with HLS support and adaptive bitrate.
```

---

## Фаза 4: Улучшение coverage

### 4.1 Анализ baseline coverage

**Ожидаемые результаты baseline (оценка):**

```
Overall coverage: ~65%

By module:
  kithara-net:       80% ✓ (хорошо протестирован)
  kithara-hls:       70% ⚠️ (много интеграционных тестов)
  kithara-decode:    75% ✓ (хорошие unit тесты)
  kithara-assets:    60% ⚠️ (нужны unit тесты для eviction logic)
  kithara-storage:   55% ❌ (мало unit тестов)
  kithara-stream:    65% ⚠️
  kithara-bufpool:   85% ✓
  kithara-worker:    70% ✓

Uncovered areas (предположительно):
  - Error handling paths
  - Edge cases в ABR logic
  - Eviction policies
  - Crypto/encryption paths
```

### 4.2 Целевые улучшения

**Приоритет 1: kithara-storage (55% → 75%)**

Добавить unit-тесты для:
- `StreamingResource::write_at()` edge cases
- `AtomicResource::replace()` error paths
- Concurrent access patterns

**Приоритет 2: kithara-assets eviction (60% → 75%)**

Добавить unit-тесты для:
- LRU eviction logic
- Pin/lease semantics
- Edge cases (empty cache, single item, etc.)

**Приоритет 3: kithara-hls ABR (70% → 85%)**

Добавить unit-тесты для:
- EWMA calculation edge cases
- Buffer thresholds
- Variant selection logic

### 4.3 Property-based testing с proptest

**Добавить в `workspace.dependencies`:**

```toml
proptest = "1.4"
```

**Примеры property tests:**

**kithara-hls ABR invariants:**

```rust
use proptest::prelude::*;

proptest! {
    #[test]
    fn abr_never_selects_out_of_bounds_variant(
        throughput in 100_000u64..10_000_000u64,
        buffer in 0.0f64..60.0f64,
        num_variants in 1usize..10
    ) {
        let variants = create_test_variants(num_variants);
        let controller = AbrController::new(AbrConfig::default(), None);

        let decision = controller.decide_with_throughput(
            &variants,
            throughput,
            buffer
        );

        // Invariant: selected variant must be valid index
        prop_assert!(decision.target_variant_index < num_variants);
    }

    #[test]
    fn abr_prefers_higher_bitrate_with_good_bandwidth(
        throughput in 5_000_000u64..50_000_000u64,
        buffer in 10.0f64..60.0f64
    ) {
        let variants = create_test_variants(5);
        let controller = AbrController::new(AbrConfig::default(), None);

        let decision = controller.decide_with_throughput(&variants, throughput, buffer);

        // Property: с хорошим throughput должны выбирать не самый низкий вариант
        prop_assert!(decision.target_variant_index > 0);
    }
}
```

**kithara-bufpool размеры:**

```rust
proptest! {
    #[test]
    fn bufpool_slices_never_exceed_capacity(
        capacity in 1024usize..1_000_000,
        slice_size in 1usize..10_000
    ) {
        let pool = BufferPool::new(capacity);
        let slice = pool.get_slice(slice_size);

        // Invariant: slice не больше capacity
        prop_assert!(slice.len() <= capacity);
    }
}
```

---

## Фаза 5: Advanced Mockall patterns

### 5.1 Mock для async streams

**Проблема:** `Net::stream()` возвращает `Pin<Box<dyn Stream>>`

**Решение:** Использовать `returning()` с async stream helpers

```rust
use futures::stream;

#[tokio::test]
async fn test_streaming_download() {
    let mut mock_net = MockNet::new();

    mock_net
        .expect_stream()
        .returning(|_, _| {
            // Create mock byte stream
            let chunks = vec![
                Ok(Bytes::from("chunk1")),
                Ok(Bytes::from("chunk2")),
                Ok(Bytes::from("chunk3")),
            ];

            Ok(Box::pin(stream::iter(chunks)) as ByteStream)
        });

    // Test streaming logic
    let mut stream = mock_net.stream(url, None).await.unwrap();
    // ... assert chunks
}
```

### 5.2 Mocking с different return values

**Scenario:** Network может возвращать разные результаты для разных URL

```rust
#[tokio::test]
async fn test_multiple_url_responses() {
    let mut mock_net = MockNet::new();

    // Master playlist
    mock_net
        .expect_get_bytes()
        .withf(|url, _| url.path() == "/master.m3u8")
        .returning(|_, _| Ok(Bytes::from(MASTER_CONTENT)));

    // Variant 0 playlist
    mock_net
        .expect_get_bytes()
        .withf(|url, _| url.path() == "/v0.m3u8")
        .returning(|_, _| Ok(Bytes::from(VARIANT0_CONTENT)));

    // Variant 1 playlist
    mock_net
        .expect_get_bytes()
        .withf(|url, _| url.path() == "/v1.m3u8")
        .returning(|_, _| Ok(Bytes::from(VARIANT1_CONTENT)));

    // Test делает все 3 запроса
}
```

### 5.3 Error injection testing

**Тестирование error recovery:**

```rust
#[tokio::test]
async fn test_network_error_retry() {
    let mut mock_net = MockNet::new();

    // First call fails, second succeeds
    mock_net
        .expect_get_bytes()
        .times(2)
        .returning({
            let mut call_count = 0;
            move |_, _| {
                call_count += 1;
                if call_count == 1 {
                    Err(NetError::Timeout)  // First call fails
                } else {
                    Ok(Bytes::from("success"))  // Second call succeeds
                }
            }
        });

    // Test retry logic
    let result = retry_fetch(&mock_net, url).await;
    assert!(result.is_ok());
}
```

---

## Метрики успеха

### Количественные метрики

**Coverage:**
- Baseline: ~65% (измерить)
- Цель Phase 1-2: 75%
- Цель Phase 3-4: 80%+
- Stretch goal: 85%

**Тесты:**
- Текущие: 360 integration + 241 unit = 601 тестов
- После mockall: +50 новых unit тестов (изоляция)
- После proptest: +20 property tests
- Цель: 670+ тестов

**Код моков:**
- Текущий ручной mock код: ~400 строк
- После mockall: ~100 строк (75% reduction)

### Качественные метрики

**Выразительность:**
- Тесты с mockall более читаемы
- Expectations явно документируют контракты
- Sequence verification делает порядок вызовов очевидным

**Maintainability:**
- Меньше boilerplate кода
- Автоматическая верификация
- Меньше шансов забыть проверить что-то

**Изоляция:**
- Unit-тесты не зависят от network/disk
- Детерминистичное поведение
- Быстрее выполняются

---

## Риски и митигация

### Риск 1: Breaking changes в публичных traits

**Проблема:** Добавление `#[cfg_attr(test, automock)]` может изменить public API

**Митигация:**
- Использовать `#[cfg_attr(test, ...)]` - видно только в test builds
- Добавить CI check что public API не изменился
- Документировать изменения в CHANGELOG

### Риск 2: Mockall не поддерживает сложные associated types

**Проблема:** `Assets` trait имеет сложные associated types

**Митигация:**
- Оставить manual mock для Assets
- Использовать mockall только для простых traits
- Документировать ограничения

### Риск 3: Tarpaulin coverage на macOS

**Проблема:** Tarpaulin имеет limited macOS support

**Митигация:**
- Запускать coverage в CI (Linux)
- Локально использовать Docker
- Альтернатива: llvm-cov (но сложнее настроить)

### Риск 4: Performance regression от mockall

**Проблема:** Моки могут замедлить тесты

**Митигация:**
- Benchmark до/после
- Использовать `cargo test --release` для быстрых тестов
- Mockall генерирует эффективный код

---

## Этапы реализации

### Неделя 1: Базовая интеграция
- [ ] Добавить mockall зависимости во ВСЕ 9 крейтов
- [ ] Провести полный аудит pub traits во всех крейтах
- [ ] Добавить `#[cfg_attr(test, automock)]` к Estimator (kithara-hls)
- [ ] Добавить `#[cfg_attr(test, automock)]` к Net (kithara-net)
- [ ] Рефакторить AbrController unit tests
- [ ] Измерить baseline coverage
- [ ] Создать tarpaulin.toml

### Неделя 2: Network layer mocking
- [ ] Добавить automock к Net trait
- [ ] Рефакторить 5-10 HLS integration tests
- [ ] Добавить sequence tests для ABR
- [ ] Добавить error injection tests

### Неделя 3: CI/CD интеграция
- [ ] Создать coverage.yml workflow
- [ ] Настроить codecov.io
- [ ] Добавить coverage badges
- [ ] Документировать процесс в CONTRIBUTING.md

### Неделя 4: Coverage improvements
- [ ] Добавить unit tests для storage (55%→75%)
- [ ] Добавить unit tests для assets eviction (60%→75%)
- [ ] Добавить property tests для ABR
- [ ] Финальная проверка coverage (цель: 80%)

---

## Ресурсы и ссылки

**Mockall:**
- [GitHub](https://github.com/asomers/mockall)
- [Docs.rs](https://docs.rs/mockall/latest/mockall/)
- [User Guide](https://docs.rs/mockall/latest/mockall/#user-guide)

**Codecov:**
- [Rust Guide](https://about.codecov.io/language/rust/)
- [Tarpaulin Integration](https://about.codecov.io/tool/tarpaulin/)
- [GitHub Actions](https://github.com/codecov/codecov-action)

**Tarpaulin:**
- [GitHub](https://github.com/xd009642/tarpaulin)
- [Configuration](https://github.com/xd009642/tarpaulin#configuration)

**Proptest:**
- [Docs](https://docs.rs/proptest/latest/proptest/)
- [Book](https://proptest-rs.github.io/proptest/)

---

## Приложение: Примеры "до/после"

### A1: ABR Controller test

**До (ручной mock):**
```rust
#[derive(Clone)]
struct MockEstimator {
    estimate: Option<u64>,
    call_count: Arc<AtomicUsize>,
}

impl Estimator for MockEstimator {
    fn estimate_bps(&self) -> Option<u64> {
        self.call_count.fetch_add(1, Ordering::SeqCst);
        self.estimate
    }

    fn push_sample(&mut self, _sample: ThroughputSample) {}
}

#[test]
fn test_estimator_called_once() {
    let mock = MockEstimator {
        estimate: Some(1_000_000),
        call_count: Arc::new(AtomicUsize::new(0)),
    };

    let controller = AbrController::with_estimator(cfg, mock.clone(), None);
    controller.decide(&variants, 10.0, now);

    assert_eq!(mock.call_count.load(Ordering::SeqCst), 1);
}
```

**После (mockall):**
```rust
use super::MockEstimator;  // Auto-generated

#[test]
fn test_estimator_called_once() {
    let mut mock = MockEstimator::new();

    mock.expect_estimate_bps()
        .times(1)  // Built-in!
        .returning(|| Some(1_000_000));

    let controller = AbrController::with_estimator(cfg, mock, None);
    controller.decide(&variants, 10.0, now);

    // Auto-verified on drop - no manual assert needed!
}
```

### A2: Network isolation

**До (реальный HTTP):**
```rust
#[tokio::test]
async fn test_fetch_master_playlist() {
    let server = TestServer::new().await;  // Реальный HTTP!
    let url = server.url("/master.m3u8").unwrap();

    let net = HttpClient::new(NetOptions::default());
    let result = net.get_bytes(url, None).await;

    assert!(result.is_ok());
}
```

**После (mock network):**
```rust
#[tokio::test]
async fn test_fetch_master_playlist() {
    let mut mock_net = MockNet::new();

    mock_net
        .expect_get_bytes()
        .withf(|url, _| url.path() == "/master.m3u8")
        .times(1)
        .returning(|_, _| Ok(Bytes::from(MASTER_PLAYLIST)));

    let result = mock_net.get_bytes(url, None).await;

    assert!(result.is_ok());
    // Mockall auto-verifies expectations
}
```

**Выгоды:**
- Нет зависимости от network stack ✓
- Тест выполняется ~100x быстрее ✓
- Детерминистичен (нет race conditions) ✓
- Легко тестировать error cases ✓

---

## Заключение

Этот план обеспечивает:

1. **Поэтапную миграцию** с минимальным риском
2. **Измеримые метрики** на каждом этапе
3. **Backward compatibility** существующих тестов
4. **Professional-grade** test infrastructure
5. **80%+ code coverage** с автоматизированным reporting

После завершения проект будет иметь:
- Меньше boilerplate mock кода
- Более выразительные тесты
- Автоматизированный coverage tracking
- Property-based testing для критических алгоритмов
- Лучшую изоляцию unit-тестов
