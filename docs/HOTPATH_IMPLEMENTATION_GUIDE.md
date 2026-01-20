# Пошаговое руководство по интеграции hotpath-rs в Kithara

**Версия:** 1.0
**Дата:** 2026-01-20
**Статус:** Ready for implementation

---

## Контекст

hotpath-rs — легковесный async Rust профайлер с нулевым overhead когда выключен.

**Репозиторий:** https://github.com/pawurb/hotpath-rs
**Версия:** 0.9

**Ключевые возможности:**
- Инструментирование функций через макрос `#[hotpath::measure]`
- Мониторинг каналов через `hotpath::channel!()`
- Профилирование памяти через `hotpath-alloc` feature
- Percentile метрики (p50, p95, p99)
- Zero overhead когда feature выключен

---

## Phase 1: Foundation (Настройка инфраструктуры)

**Цель:** Добавить hotpath в workspace и создать базовую инфраструктуру для тестов.

**Время:** 1-2 часа

### Step 1.1: Добавить workspace dependency

**Файл:** `Cargo.toml` (корневой)

**Действие:** Добавить в секцию `[workspace.dependencies]`:

```toml
[workspace.dependencies]
# ... существующие зависимости ...
hotpath = "0.9"
```

**Местоположение:** После других зависимостей, перед секцией `[workspace.lints]`.

**Проверка:**
```bash
cargo tree -p kithara-worker | grep hotpath
# Пока ничего не должно показать
```

---

### Step 1.2: Добавить feature flags в kithara-worker

**Файл:** `crates/kithara-worker/Cargo.toml`

**Действие 1:** Добавить optional dependency в `[dependencies]`:

```toml
[dependencies]
# ... существующие зависимости ...
hotpath = { workspace = true, optional = true }
```

**Действие 2:** Добавить dev-dependency:

```toml
[dev-dependencies]
# ... существующие зависимости ...
hotpath = { workspace = true }
```

**Действие 3:** Добавить features в конец файла:

```toml
[features]
default = []
hotpath = ["dep:hotpath"]
hotpath-alloc = ["hotpath", "hotpath/hotpath-alloc"]
```

**Проверка:**
```bash
cargo build -p kithara-worker --features hotpath
cargo build -p kithara-worker --features hotpath-alloc
cargo build -p kithara-worker  # без features
```

Все три команды должны успешно скомпилироваться.

---

### Step 1.3: Создать hotpath helper для тестов

**Файл:** `tests/helpers/mod.rs` (новый файл или дополнить существующий)

**Действие:** Добавить модуль:

```rust
#[cfg(feature = "hotpath")]
pub mod hotpath_helpers {
    /// Create hotpath guard for unit tests.
    ///
    /// Usage:
    /// ```ignore
    /// #[tokio::test(flavor = "current_thread")]
    /// async fn my_test() {
    ///     let _guard = hotpath_guard("my_test");
    ///     // test code
    /// }
    /// ```
    pub fn hotpath_guard(name: &str) -> hotpath::FunctionsGuard {
        hotpath::FunctionsGuardBuilder::new(name)
            .percentiles(&[50, 90, 95, 99])
            .format(hotpath::Format::Table)
            .build()
    }

    /// Create hotpath guard with custom percentiles.
    pub fn hotpath_guard_with_percentiles(
        name: &str,
        percentiles: &[u8],
    ) -> hotpath::FunctionsGuard {
        hotpath::FunctionsGuardBuilder::new(name)
            .percentiles(percentiles)
            .format(hotpath::Format::Table)
            .build()
    }
}

#[cfg(not(feature = "hotpath"))]
pub mod hotpath_helpers {
    /// No-op when hotpath is disabled.
    pub struct NoOpGuard;

    pub fn hotpath_guard(_name: &str) -> NoOpGuard {
        NoOpGuard
    }

    pub fn hotpath_guard_with_percentiles(_name: &str, _percentiles: &[u8]) -> NoOpGuard {
        NoOpGuard
    }
}
```

**Проверка:**
```bash
# Должно скомпилироваться с и без feature
cargo build -p kithara-integration-tests --features hotpath
cargo build -p kithara-integration-tests
```

---

### Step 1.4: Обновить документацию

**Файл:** `CLAUDE.md`

**Действие:** Добавить новую секцию после "## Commands":

```markdown
## Performance Profiling

Profile code with hotpath-rs for performance analysis.

### Running profiled tests

```bash
# Single test with profiling
cargo test --features hotpath test_async_worker_throughput -- --test-threads=1

# All profiling tests
cargo test --features hotpath --test performance_profiling -- --test-threads=1

# Specific crate
cargo test --features hotpath -p kithara-worker -- --test-threads=1

# With memory profiling (requires single-threaded runtime)
cargo test --features hotpath,hotpath-alloc test_name -- --test-threads=1
```

**Important:** Tests must run with `--test-threads=1` for accurate metrics.

### Adding instrumentation

```rust
// Function instrumentation
#[cfg_attr(feature = "hotpath", hotpath::measure)]
async fn my_function() {
    // code
}

// Channel monitoring
#[cfg(feature = "hotpath")]
let (tx, rx) = hotpath::channel!(
    mpsc::channel(4),
    label = "my_channel"
);

#[cfg(not(feature = "hotpath"))]
let (tx, rx) = mpsc::channel(4);

// Block profiling
#[cfg(feature = "hotpath")]
hotpath::measure_block!("my_block", {
    // code to profile
});

#[cfg(not(feature = "hotpath"))]
{
    // code to profile
}
```

See `docs/HOTPATH_INTEGRATION_PLAN.md` for detailed instrumentation guide.
```

**Проверка:**
Визуально проверить что markdown правильно форматируется.

---

### Phase 1 Checklist

- [ ] hotpath добавлен в workspace dependencies
- [ ] feature flags добавлены в kithara-worker/Cargo.toml
- [ ] hotpath_helpers модуль создан
- [ ] CLAUDE.md обновлен
- [ ] Все билды проходят с и без features
- [ ] `cargo test --workspace` проходит без изменений

**Критерий успеха Phase 1:** Проект компилируется с `--features hotpath` без ошибок, существующие тесты не сломаны.

---

## Phase 2: First Instrumentation (Первый рабочий пример)

**Цель:** Создать один работающий пример профилирования в kithara-worker.

**Время:** 1-2 часа

### Step 2.1: Инструментировать AsyncWorker::run_worker

**Файл:** `crates/kithara-worker/src/async_worker.rs`

**Действие 1:** Добавить атрибут к функции `run_worker`:

Найти:
```rust
    /// Run the async worker loop.
    async fn run_worker(mut self) {
```

Заменить на:
```rust
    /// Run the async worker loop.
    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    async fn run_worker(mut self) {
```

**Действие 2:** Добавить инструментирование в select! loop.

Найти блок:
```rust
                fetch = self.source.fetch_next(), if !at_eof => {
                    let is_eof = fetch.is_eof();
                    let epoch = fetch.epoch();

                    if is_eof {
                        trace!(epoch, "AsyncWorker: EOF reached");
                    }

                    let (send_result, cmd_received) = self.send_item(fetch.data, is_eof).await;
```

Обернуть fetch в measure_block:
```rust
                fetch = self.source.fetch_next(), if !at_eof => {
                    let is_eof = fetch.is_eof();
                    let epoch = fetch.epoch();

                    if is_eof {
                        trace!(epoch, "AsyncWorker: EOF reached");
                    }

                    #[cfg(feature = "hotpath")]
                    let (send_result, cmd_received) = hotpath::measure_block!("send_item", {
                        self.send_item(fetch.data, is_eof).await
                    });

                    #[cfg(not(feature = "hotpath"))]
                    let (send_result, cmd_received) = self.send_item(fetch.data, is_eof).await;
```

**Проверка:**
```bash
cargo build -p kithara-worker --features hotpath
cargo test -p kithara-worker --features hotpath
```

---

### Step 2.2: Создать первый профилирующий тест

**Файл:** `tests/tests/worker.rs`

**Действие:** Добавить в конец файла новый тест:

```rust
#[cfg(feature = "hotpath")]
mod profiling_tests {
    use super::*;
    use crate::helpers::hotpath_helpers::hotpath_guard;

    #[tokio::test(flavor = "current_thread")]
    async fn profile_async_worker_throughput() {
        let _guard = hotpath_guard("async_worker_throughput");

        // Create source with 1000 items
        let source = TestAsyncSource::new((0..1000).map(|i| i as i32).collect());
        let (cmd_tx, cmd_rx) = mpsc::channel(4);

        #[cfg(feature = "hotpath")]
        let (data_tx, data_rx) = hotpath::channel!(
            kanal::bounded_async(16),
            label = "worker_data"
        );

        #[cfg(not(feature = "hotpath"))]
        let (data_tx, data_rx) = kanal::bounded_async(16);

        let worker = AsyncWorker::new(source, cmd_rx, data_tx);
        tokio::spawn(worker.run());

        // Consume all items
        let mut count = 0;
        while let Ok(item) = data_rx.recv().await {
            count += 1;
            if item.is_eof {
                break;
            }
        }

        assert_eq!(count, 1001); // 1000 items + 1 EOF
        drop(cmd_tx); // Clean shutdown
    }
}
```

**Проверка:**
```bash
# Должен запуститься и показать таблицу с метриками
cargo test --features hotpath -p kithara-integration-tests profile_async_worker_throughput -- --test-threads=1 --nocapture
```

**Ожидаемый вывод:** Таблица с функциями `run_worker`, `send_item` и их метриками.

---

### Step 2.3: Проверить zero-overhead без feature

**Проверка 1:** Тест должен проходить без hotpath:
```bash
cargo test -p kithara-integration-tests test_async_worker_basic
```

**Проверка 2:** Код должен компилироваться одинаково:
```bash
# С feature
cargo build -p kithara-worker --features hotpath --release --verbose 2>&1 | grep -c hotpath

# Без feature
cargo build -p kithara-worker --release --verbose 2>&1 | grep -c hotpath
# Должно быть 0
```

---

### Phase 2 Checklist

- [ ] AsyncWorker::run_worker инструментирован
- [ ] Первый профилирующий тест создан
- [ ] Тест проходит с `--features hotpath`
- [ ] Тест показывает таблицу метрик
- [ ] Существующие тесты не сломаны
- [ ] Без feature нет overhead (проверено компиляцией)

**Критерий успеха Phase 2:** Один работающий тест показывает метрики производительности AsyncWorker.

---

## Phase 3: Extended Instrumentation (Расширение покрытия)

**Цель:** Добавить инструментирование в ключевые крейты.

**Время:** 3-4 часа

### Step 3.1: Инструментировать SyncWorker

**Файл:** `crates/kithara-worker/src/sync_worker.rs`

**Действие:** Добавить атрибут к `run_worker`:

```rust
    /// Run the blocking worker loop.
    ///
    /// This should be called inside a `spawn_blocking` task.
    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    fn run_worker(mut self) {
```

**Тест:** `tests/tests/worker.rs`

```rust
    #[tokio::test(flavor = "current_thread")]
    async fn profile_sync_worker_throughput() {
        let _guard = hotpath_guard("sync_worker_throughput");

        let source = TestSyncSource::new((0..1000).map(|i| i as i32).collect());
        let (cmd_tx, cmd_rx) = mpsc::channel(4);

        #[cfg(feature = "hotpath")]
        let (data_tx, data_rx) = hotpath::channel!(
            kanal::bounded(16),
            label = "sync_worker_data"
        );

        #[cfg(not(feature = "hotpath"))]
        let (data_tx, data_rx) = kanal::bounded(16);

        let worker = SyncWorker::new(source, cmd_rx, data_tx);
        tokio::spawn(worker.run());

        let async_rx = data_rx.as_async();
        let mut count = 0;
        while let Ok(item) = async_rx.recv().await {
            count += 1;
            if item.is_eof {
                break;
            }
        }

        assert_eq!(count, 1001);
        drop(cmd_tx);
    }
```

**Проверка:**
```bash
cargo test --features hotpath -p kithara-integration-tests profile_sync_worker_throughput -- --test-threads=1 --nocapture
```

---

### Step 3.2: Добавить features в другие крейты

Повторить Step 1.2 для следующих крейтов:

**Крейты для инструментирования:**
1. `kithara-decode/Cargo.toml`
2. `kithara-stream/Cargo.toml`
3. `kithara-hls/Cargo.toml` (опционально)
4. `kithara-net/Cargo.toml` (опционально)

**Шаблон для каждого крейта:**

```toml
[dependencies]
hotpath = { workspace = true, optional = true }

[dev-dependencies]
hotpath = { workspace = true }

[features]
default = []
hotpath = ["dep:hotpath"]
hotpath-alloc = ["hotpath", "hotpath/hotpath-alloc"]
```

**Проверка после каждого:**
```bash
cargo build -p <crate-name> --features hotpath
```

---

### Step 3.3: Инструментировать kithara-decode

**Файл:** `crates/kithara-decode/src/pipeline.rs`

**Действие 1:** Инструментировать `DecodeSource::fetch_next`:

```rust
impl<D: Decoder> SyncWorkerSource for DecodeSource<D> {
    type Chunk = PcmChunk<f32>;
    type Command = PipelineCommand;

    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    fn fetch_next(&mut self) -> Fetch<Self::Chunk> {
        loop {
            // Decode next chunk
            #[cfg(feature = "hotpath")]
            let decode_result = hotpath::measure_block!("decoder_next_chunk", {
                self.decoder.next_chunk()
            });

            #[cfg(not(feature = "hotpath"))]
            let decode_result = self.decoder.next_chunk();

            match decode_result {
                Ok(Some(decoded_chunk)) => {
                    trace!(frames = decoded_chunk.frames(), "Decoded chunk");

                    // Resample if resampler exists
                    match &mut self.resampler {
                        Some(resampler) => {
                            #[cfg(feature = "hotpath")]
                            let resampled = hotpath::measure_block!("resampler_process", {
                                resampler.process(&decoded_chunk)
                            });

                            #[cfg(not(feature = "hotpath"))]
                            let resampled = resampler.process(&decoded_chunk);

                            if let Some(resampled) = resampled {
                                return Fetch::new(resampled, false, 0);
                            }
                            continue;
                        }
                        None => {
                            return Fetch::new(decoded_chunk, false, 0);
                        }
                    }
                }
                Ok(None) => {
                    // EOF - flush resampler
                    if let Some(resampler) = &mut self.resampler {
                        if let Some(final_chunk) = resampler.flush() {
                            info!("Pipeline: flushing resampler");
                            return Fetch::new(final_chunk, true, 0);
                        }
                    }
                    info!("Pipeline: end of stream");
                    return Fetch::new(PcmChunk::new(self.output_spec, Vec::new()), true, 0);
                }
                Err(e) => {
                    error!(err = %e, "Pipeline decode error");
                    return Fetch::new(PcmChunk::new(self.output_spec, Vec::new()), true, 0);
                }
            }
        }
    }

    // ... остальное без изменений
}
```

**Проверка:**
```bash
cargo build -p kithara-decode --features hotpath
cargo test -p kithara-decode --features hotpath
```

---

### Step 3.4: Инструментировать kithara-stream

**Файл:** `crates/kithara-stream/src/source.rs`

**Действие:** Инструментировать `BytePrefetchSource::fetch_next`:

```rust
#[async_trait]
impl<S> kithara_worker::AsyncWorkerSource for BytePrefetchSource<S>
where
    S: Source<Item = u8>,
{
    type Chunk = ByteChunk;
    type Command = ByteSeekCmd;

    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    async fn fetch_next(&mut self) -> Fetch<Self::Chunk> {
        let range = self.read_pos..self.read_pos.saturating_add(self.chunk_size as u64);

        debug!(
            start = range.start,
            end = range.end,
            epoch = self.epoch,
            "BytePrefetchSource: waiting for range"
        );

        #[cfg(feature = "hotpath")]
        let wait_result = hotpath::measure_block!("wait_range", {
            self.src.wait_range(range).await
        });

        #[cfg(not(feature = "hotpath"))]
        let wait_result = self.src.wait_range(range).await;

        match wait_result {
            Ok(WaitOutcome::Ready) => {}
            Ok(WaitOutcome::Eof) => {
                trace!(pos = self.read_pos, epoch = self.epoch, "BytePrefetchSource: EOF");
                return Fetch::new(
                    ByteChunk {
                        file_pos: self.read_pos,
                        data: Vec::new(),
                    },
                    true,
                    self.epoch,
                );
            }
            Err(e) => {
                tracing::error!(err = %e, "BytePrefetchSource: wait_range error");
                return Fetch::new(
                    ByteChunk {
                        file_pos: self.read_pos,
                        data: Vec::new(),
                    },
                    true,
                    self.epoch,
                );
            }
        }

        // Read data
        let mut buf = vec![0u8; self.chunk_size];

        #[cfg(feature = "hotpath")]
        let read_result = hotpath::measure_block!("read_at", {
            self.src.read_at(self.read_pos, &mut buf).await
        });

        #[cfg(not(feature = "hotpath"))]
        let read_result = self.src.read_at(self.read_pos, &mut buf).await;

        match read_result {
            Ok(n) if n > 0 => {
                buf.truncate(n);
                let chunk = ByteChunk {
                    file_pos: self.read_pos,
                    data: buf,
                };
                trace!(
                    file_pos = self.read_pos,
                    bytes = n,
                    epoch = self.epoch,
                    "BytePrefetchSource: fetched"
                );
                self.read_pos = self.read_pos.saturating_add(n as u64);
                Fetch::new(chunk, false, self.epoch)
            }
            Ok(_) => {
                trace!(pos = self.read_pos, "BytePrefetchSource: EOF (read 0)");
                Fetch::new(
                    ByteChunk {
                        file_pos: self.read_pos,
                        data: Vec::new(),
                    },
                    true,
                    self.epoch,
                )
            }
            Err(e) => {
                tracing::error!(err = %e, "BytePrefetchSource: read_at error");
                Fetch::new(
                    ByteChunk {
                        file_pos: self.read_pos,
                        data: Vec::new(),
                    },
                    true,
                    self.epoch,
                )
            }
        }
    }

    // ... остальное без изменений
}
```

**Проверка:**
```bash
cargo build -p kithara-stream --features hotpath
cargo test -p kithara-stream --features hotpath
```

---

### Phase 3 Checklist

- [ ] SyncWorker инструментирован и протестирован
- [ ] Feature flags добавлены в decode, stream, hls, net
- [ ] DecodeSource::fetch_next инструментирован
- [ ] BytePrefetchSource::fetch_next инструментирован
- [ ] Все крейты компилируются с --features hotpath
- [ ] Существующие тесты проходят без регрессий

**Критерий успеха Phase 3:** Ключевые функции в worker, decode и stream инструментированы и показывают метрики.

---

## Phase 4: Integration Tests (Комплексные тесты)

**Цель:** Создать integration тесты для end-to-end профилирования.

**Время:** 2-3 часа

### Step 4.1: Создать файл для integration тестов

**Файл:** `tests/tests/performance_profiling.rs` (новый файл)

**Содержимое:**

```rust
//! Performance profiling integration tests.
//!
//! Run with: cargo test --features hotpath --test performance_profiling -- --test-threads=1

#![cfg(feature = "hotpath")]

use std::sync::Arc;
use kithara_worker::{AsyncWorker, SyncWorker, Worker};
use kithara_decode::{PipelineBuilder, SymphoniaDecoder};
use kithara_stream::{Source, SyncReader, SyncReaderParams};
use tokio::sync::mpsc;

mod helpers;
use helpers::hotpath_helpers::hotpath_guard;

// ============================================================================
// Worker Profiling Tests
// ============================================================================

#[tokio::test(flavor = "current_thread")]
async fn profile_full_decode_pipeline() {
    let _guard = hotpath_guard("full_decode_pipeline");

    // TODO: Implement with real test MP3 file
    // 1. Create SyncReader from test file
    // 2. Create SymphoniaDecoder
    // 3. Create Pipeline with resampling
    // 4. Decode several chunks
    // 5. Verify metrics are collected

    // Placeholder
    assert!(true, "To be implemented");
}

#[tokio::test(flavor = "current_thread")]
async fn profile_stream_with_seeks() {
    let _guard = hotpath_guard("stream_with_seeks");

    // TODO: Implement
    // 1. Create byte source with test data
    // 2. Create SyncReader
    // 3. Perform multiple seeks (forward/backward)
    // 4. Read after each seek
    // 5. Measure seek latency

    assert!(true, "To be implemented");
}

// ============================================================================
// Memory Profiling Tests (requires hotpath-alloc feature)
// ============================================================================

#[cfg(feature = "hotpath-alloc")]
#[tokio::test(flavor = "current_thread")]
async fn profile_decode_memory_allocations() {
    let _guard = hotpath_guard("decode_memory");

    // TODO: Implement
    // 1. Setup decoder
    // 2. Decode chunks and track allocations
    // 3. Verify allocations per chunk are reasonable

    assert!(true, "To be implemented");
}
```

**Проверка:**
```bash
# Должен скомпилироваться и показать pending тесты
cargo test --features hotpath --test performance_profiling -- --test-threads=1
```

---

### Step 4.2: Создать тестовые данные (опционально)

**Директория:** `tests/fixtures/audio/`

**Файлы для добавления (генерация или скачивание):**
- `test_short.mp3` - короткий MP3 файл (~1-3 сек, ~50KB)
- `test_medium.mp3` - средний MP3 файл (~30 сек, ~500KB)

**Генерация с ffmpeg (если доступен):**
```bash
# Генерация синусоиды 440Hz, 3 секунды
ffmpeg -f lavfi -i "sine=frequency=440:duration=3" -c:a libmp3lame -b:a 128k tests/fixtures/audio/test_short.mp3

# Генерация 30 секунд
ffmpeg -f lavfi -i "sine=frequency=440:duration=30" -c:a libmp3lame -b:a 128k tests/fixtures/audio/test_medium.mp3
```

**Альтернатива:** Использовать существующие fixture файлы из тестов.

---

### Step 4.3: Реализовать один integration тест

**Файл:** `tests/tests/performance_profiling.rs`

**Действие:** Заменить placeholder для `profile_full_decode_pipeline`:

```rust
#[tokio::test(flavor = "current_thread")]
async fn profile_full_decode_pipeline() {
    let _guard = hotpath_guard("full_decode_pipeline");

    // Find existing test MP3 from fixtures
    let test_file = std::path::PathBuf::from("tests/fixtures/audio/test_short.mp3");

    if !test_file.exists() {
        eprintln!("Skipping test: test file not found");
        return;
    }

    // Create file source
    let file_source = kithara_file::FileSource::new(test_file.clone())
        .await
        .expect("Failed to open test file");

    let source = Arc::new(file_source);

    // Create SyncReader
    let reader = SyncReader::new(source.clone(), SyncReaderParams::default());

    // Create decoder
    let decoder = SymphoniaDecoder::new(Box::new(reader))
        .expect("Failed to create decoder");

    // Decode several chunks
    let mut chunk_count = 0;
    loop {
        match decoder.next_chunk() {
            Ok(Some(_chunk)) => {
                chunk_count += 1;
                if chunk_count >= 10 {
                    break; // Decode first 10 chunks
                }
            }
            Ok(None) => break, // EOF
            Err(e) => {
                eprintln!("Decode error: {}", e);
                break;
            }
        }
    }

    assert!(chunk_count > 0, "Should decode at least one chunk");
    println!("Decoded {} chunks", chunk_count);
}
```

**Проверка:**
```bash
cargo test --features hotpath --test performance_profiling profile_full_decode_pipeline -- --test-threads=1 --nocapture
```

**Ожидаемый результат:** Таблица с метриками функций decode pipeline.

---

### Phase 4 Checklist

- [ ] performance_profiling.rs создан
- [ ] Тестовые MP3 файлы добавлены (или используются существующие)
- [ ] profile_full_decode_pipeline реализован и работает
- [ ] Тест показывает метрики decode + resampler
- [ ] Остальные тесты оставлены как TODO для будущей работы

**Критерий успеха Phase 4:** Один комплексный integration тест работает и показывает метрики всего decode pipeline.

---

## Phase 5: Documentation & Baselines (Документация)

**Цель:** Задокументировать использование и собрать baseline метрики.

**Время:** 1 час

### Step 5.1: Создать PROFILING.md

**Файл:** `docs/PROFILING.md` (новый)

**Содержимое:**

```markdown
# Performance Profiling Guide

## Overview

Kithara uses [hotpath-rs](https://github.com/pawurb/hotpath-rs) for performance profiling with zero overhead when disabled.

## Running Profiled Tests

```bash
# Single test
cargo test --features hotpath test_name -- --test-threads=1 --nocapture

# All profiling tests
cargo test --features hotpath --test performance_profiling -- --test-threads=1

# Specific crate
cargo test --features hotpath -p kithara-worker -- --test-threads=1

# With memory profiling
cargo test --features hotpath,hotpath-alloc test_name -- --test-threads=1
```

**Important:** Always use `--test-threads=1` for accurate metrics.

## Interpreting Results

hotpath shows percentile-based latency metrics:

```
Function          | Calls | P50    | P90    | P95    | P99    | Total
------------------|-------|--------|--------|--------|--------|--------
run_worker        |   1   | 1.2ms  | -      | -      | -      | 1.2ms
send_item         | 1000  | 12µs   | 18µs   | 23µs   | 45µs   | 15.3ms
decoder_next_chunk|  500  | 250µs  | 320µs  | 380µs  | 520µs  | 142ms
```

**Key metrics:**
- **Calls:** Number of invocations
- **P50/P90/P95/P99:** Percentile latencies
- **Total:** Cumulative time spent in function

## Adding Instrumentation

### Function instrumentation

```rust
#[cfg_attr(feature = "hotpath", hotpath::measure)]
async fn my_function() {
    // code
}
```

### Block instrumentation

```rust
#[cfg(feature = "hotpath")]
let result = hotpath::measure_block!("my_block", {
    expensive_operation()
});

#[cfg(not(feature = "hotpath"))]
let result = expensive_operation();
```

### Channel monitoring

```rust
#[cfg(feature = "hotpath")]
let (tx, rx) = hotpath::channel!(
    mpsc::channel(4),
    label = "my_channel"
);

#[cfg(not(feature = "hotpath"))]
let (tx, rx) = mpsc::channel(4);
```

## Best Practices

1. **Always use current_thread runtime for tests:**
   ```rust
   #[tokio::test(flavor = "current_thread")]
   async fn my_profiled_test() { ... }
   ```

2. **Create guard at test start:**
   ```rust
   let _guard = hotpath_guard("test_name");
   ```

3. **Run tests isolated:**
   ```bash
   cargo test --features hotpath test_name -- --test-threads=1
   ```

4. **Memory profiling requires single-threaded:**
   ```bash
   cargo test --features hotpath,hotpath-alloc -- --test-threads=1
   ```

## Known Limitations

- Memory profiling (`hotpath-alloc`) requires single-threaded tokio runtime
- Profiling adds small overhead even when enabled (typically <1%)
- CI environments may show unstable timings due to shared resources
- Not suitable for production deployments (use for development/testing only)

## Current Instrumentation

### kithara-worker
- `AsyncWorker::run_worker()` - Main async worker loop
- `SyncWorker::run_worker()` - Main sync worker loop
- `send_item` block - Item sending with backpressure

### kithara-decode
- `DecodeSource::fetch_next()` - Decode + resample pipeline
- `decoder_next_chunk` block - Raw decoder operation
- `resampler_process` block - Resampling operation

### kithara-stream
- `BytePrefetchSource::fetch_next()` - Prefetch coordination
- `wait_range` block - Waiting for data availability
- `read_at` block - Reading from source

## Baseline Metrics

TODO: Collect baseline metrics after initial implementation.

Run this to generate baselines:
```bash
cargo test --features hotpath --test performance_profiling -- --test-threads=1 --nocapture 2>&1 | tee profiling_baseline.txt
```

## Future Work

- [ ] Add more integration tests for HLS playback
- [ ] Profile network layer (kithara-net)
- [ ] Create performance regression test suite for CI
- [ ] Collect comprehensive baselines for all instrumented functions
- [ ] Add benchmarks with Criterion integration
```

---

### Step 5.2: Собрать baseline метрики

**Действие:** Запустить все профилирующие тесты и сохранить результаты:

```bash
# Создать директорию для метрик
mkdir -p docs/baselines

# Запустить тесты и сохранить вывод
cargo test --features hotpath --test performance_profiling -- --test-threads=1 --nocapture 2>&1 | tee docs/baselines/baseline_$(date +%Y%m%d).txt

# Также для unit тестов
cargo test --features hotpath -p kithara-worker -- --test-threads=1 --nocapture 2>&1 | tee -a docs/baselines/baseline_$(date +%Y%m%d).txt
```

**Добавить в .gitignore:**
```
docs/baselines/*.txt
```

---

### Phase 5 Checklist

- [ ] docs/PROFILING.md создан
- [ ] Baseline метрики собраны
- [ ] .gitignore обновлен для baselines
- [ ] Документация проверена на читаемость
- [ ] Все ссылки в документации валидны

**Критерий успеха Phase 5:** Полная документация по использованию hotpath в проекте.

---

## Final Verification (Финальная проверка)

### Полный цикл тестирования

```bash
# 1. Clean build без features
cargo clean
cargo build --workspace
cargo test --workspace

# 2. Build с hotpath
cargo build --workspace --features hotpath

# 3. Run profiling tests
cargo test --features hotpath -p kithara-integration-tests profile_async_worker_throughput -- --test-threads=1 --nocapture

cargo test --features hotpath -p kithara-integration-tests profile_sync_worker_throughput -- --test-threads=1 --nocapture

cargo test --features hotpath --test performance_profiling -- --test-threads=1 --nocapture

# 4. Verify zero overhead (timing existing test suite)
time cargo test --workspace --quiet
time cargo test --workspace --features hotpath --quiet
# Времена должны быть примерно одинаковыми
```

### Проверка документации

- [ ] README.md не сломан
- [ ] CLAUDE.md содержит секцию про hotpath
- [ ] docs/PROFILING.md корректен
- [ ] docs/HOTPATH_INTEGRATION_PLAN.md актуален
- [ ] docs/HOTPATH_TESTS_INVENTORY.md актуален

### Code review checklist

- [ ] Все `#[cfg(feature = "hotpath")]` paired с `#[cfg(not(feature = "hotpath"))]`
- [ ] Нет unwrap/expect в instrumentation коде
- [ ] Channel monitoring использует label для важных каналов
- [ ] Функции с `#[hotpath::measure]` имеют осмысленные имена
- [ ] Block measurements имеют короткие лейблы (<20 символов)
- [ ] Нет дублирования кода между cfg branches

---

## Troubleshooting

### Проблема: Тесты не показывают метрики

**Решение:**
```bash
# Убедитесь что feature включен
cargo test --features hotpath test_name -- --test-threads=1 --nocapture

# Проверьте что guard создается:
let _guard = hotpath_guard("test_name");
```

### Проблема: "no method named `run`"

**Причина:** Забыли импортировать Worker trait.

**Решение:**
```rust
use kithara_worker::Worker;
```

### Проблема: Compilation errors с hotpath макросами

**Причина:** Функция не async или неправильный атрибут.

**Решение:**
```rust
// Для async функций
#[cfg_attr(feature = "hotpath", hotpath::measure)]
async fn my_async_fn() { }

// Для sync функций
#[cfg_attr(feature = "hotpath", hotpath::measure)]
fn my_sync_fn() { }
```

### Проблема: Memory profiling не работает

**Причина:** Требуется current_thread runtime.

**Решение:**
```rust
#[tokio::test(flavor = "current_thread")]
async fn my_test() { }
```

И запуск:
```bash
cargo test --features hotpath,hotpath-alloc -- --test-threads=1
```

---

## Next Steps After Implementation

### Short term (1-2 weeks)
1. Собрать полные baseline метрики для всех функций
2. Добавить оставшиеся integration тесты из HOTPATH_TESTS_INVENTORY.md
3. Инструментировать kithara-hls и kithara-net
4. Создать dashboard для метрик (опционально)

### Medium term (1 month)
1. Интегрировать profiling в CI/CD
2. Создать performance regression tests
3. Оптимизировать выявленные bottlenecks
4. Добавить Criterion benchmarks

### Long term (3+ months)
1. Continuous performance monitoring
2. Historical baselines tracking
3. Automated performance reports
4. Integration с grafana/prometheus (опционально)

---

## Summary

**Phases:**
1. ✅ Foundation (1-2h) - Setup dependencies и infrastructure
2. ✅ First Instrumentation (1-2h) - Первый рабочий пример в AsyncWorker
3. ✅ Extended Instrumentation (3-4h) - Покрытие worker, decode, stream
4. ✅ Integration Tests (2-3h) - End-to-end профилирование
5. ✅ Documentation (1h) - Полная документация и baselines

**Total estimated time:** 8-12 hours

**Success criteria:**
- ✅ Проект компилируется с/без `--features hotpath`
- ✅ Минимум 3 работающих профилирующих теста
- ✅ Инструментирование в worker, decode, stream крейтах
- ✅ Один integration тест показывает end-to-end метрики
- ✅ Полная документация в docs/PROFILING.md
- ✅ Существующие тесты не сломаны

**Key files created/modified:**
- ✅ Root Cargo.toml (workspace dependency)
- ✅ kithara-worker/Cargo.toml (features)
- ✅ tests/helpers/mod.rs (hotpath_helpers)
- ✅ CLAUDE.md (usage section)
- ✅ crates/kithara-worker/src/async_worker.rs (instrumentation)
- ✅ crates/kithara-worker/src/sync_worker.rs (instrumentation)
- ✅ crates/kithara-decode/src/pipeline.rs (instrumentation)
- ✅ crates/kithara-stream/src/source.rs (instrumentation)
- ✅ tests/tests/worker.rs (profiling tests)
- ✅ tests/tests/performance_profiling.rs (integration tests)
- ✅ docs/PROFILING.md (guide)

---

**Готово к реализации!** Следуйте phases step-by-step, проверяя каждый checklist.
