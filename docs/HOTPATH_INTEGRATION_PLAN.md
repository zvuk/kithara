# План интеграции hotpath-rs в проект Kithara

## Обзор

hotpath-rs — это легковесный async Rust профайлер для выявления узких мест производительности. Интеграция позволит:
- Отслеживать время выполнения критичных функций
- Мониторить пропускную способность каналов
- Профилировать аллокации памяти
- Выявлять медленные участки кода в CI/CD

## 1. Зависимости и feature flags

### 1.1 Добавить в корневой `Cargo.toml`

```toml
[workspace.dependencies]
hotpath = "0.9"

[workspace.metadata.features]
# Performance profiling features (disabled by default)
hotpath = []
hotpath-alloc = []
```

### 1.2 Обновить `Cargo.toml` крейтов с инструментацией

Для каждого крейта, который будет профилироваться:

```toml
[dependencies]
hotpath = { workspace = true, optional = true }

[dev-dependencies]
hotpath = { workspace = true }

[features]
hotpath = ["dep:hotpath"]
hotpath-alloc = ["hotpath", "hotpath/hotpath-alloc"]
```

**Приоритетные крейты:**
- `kithara-worker` — core worker loop
- `kithara-decode` — CPU-intensive декодирование
- `kithara-stream` — byte streaming и prefetch
- `kithara-hls` — network координация
- `kithara-net` — HTTP операции

## 2. Целевые области для инструментирования

### 2.1 kithara-worker

#### AsyncWorker::run_worker()
```rust
#[cfg_attr(feature = "hotpath", hotpath::measure)]
async fn run_worker(mut self) {
    // ...
}
```

**Метрики:**
- Время цикла worker loop
- Частота получения команд
- Задержка между fetch операциями

#### SyncWorker::run_worker()
```rust
#[cfg_attr(feature = "hotpath", hotpath::measure)]
fn run_worker(mut self) {
    // ...
}
```

**Метрики:**
- Blocking операции
- Частота команд в sync режиме
- Backpressure на kanal channel

#### Каналы
```rust
#[cfg(feature = "hotpath")]
let (cmd_tx, cmd_rx) = hotpath::channel!(
    mpsc::channel::<S::Command>(4),
    label = "worker_commands"
);

#[cfg(not(feature = "hotpath"))]
let (cmd_tx, cmd_rx) = mpsc::channel::<S::Command>(4);
```

### 2.2 kithara-decode

#### DecodeSource::fetch_next()
```rust
#[cfg_attr(feature = "hotpath", hotpath::measure)]
fn fetch_next(&mut self) -> Fetch<Self::Chunk> {
    #[cfg(feature = "hotpath")]
    hotpath::measure_block!("decode_chunk", {
        // decoder logic
    });

    #[cfg(not(feature = "hotpath"))]
    {
        // decoder logic
    }
}
```

**Метрики:**
- Время декодирования одного chunk
- Overhead resampler
- Flush resampler latency

#### ResamplerProcessor::process()
```rust
#[cfg_attr(feature = "hotpath", hotpath::measure)]
pub fn process(&mut self, input: &PcmChunk<f32>) -> Option<PcmChunk<f32>> {
    // ...
}
```

### 2.3 kithara-stream

#### BytePrefetchSource::fetch_next()
```rust
#[cfg_attr(feature = "hotpath", hotpath::measure)]
async fn fetch_next(&mut self) -> Fetch<Self::Chunk> {
    #[cfg(feature = "hotpath")]
    let wait_time = hotpath::measure_block!("wait_range", {
        self.src.wait_range(range).await
    });

    #[cfg(not(feature = "hotpath"))]
    let wait_time = self.src.wait_range(range).await;

    // ...
}
```

**Метрики:**
- Задержка wait_range
- Скорость read_at
- Размер и частота chunks

#### SyncReader::recv_chunk()
```rust
#[cfg_attr(feature = "hotpath", hotpath::measure)]
fn recv_chunk(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
    // ...
}
```

### 2.4 kithara-hls

#### HlsDriver координация
```rust
#[cfg_attr(feature = "hotpath", hotpath::measure)]
pub async fn run(&mut self) -> Result<(), HlsError> {
    // ...
}
```

#### Segment fetch
```rust
#[cfg_attr(feature = "hotpath", hotpath::measure)]
async fn fetch_segment(&self, segment_ref: SegmentRef) -> Result<(), HlsError> {
    // ...
}
```

**Метрики:**
- Время загрузки сегмента
- Throughput ABR контроллера
- Задержка при переключении вариантов

### 2.5 kithara-net

#### HttpClient::get_stream()
```rust
#[cfg_attr(feature = "hotpath", hotpath::measure)]
pub async fn get_stream(&self, url: &str) -> NetResult<Response> {
    // ...
}
```

**Метрики:**
- Latency HTTP запросов
- Retry overhead
- Connection reuse эффективность

## 3. Юнит тесты с профилированием

### 3.1 Шаблон для unit тестов

Создать файл `tests/helpers/hotpath_helpers.rs`:

```rust
#[cfg(feature = "hotpath")]
pub fn hotpath_guard(name: &str) -> hotpath::FunctionsGuard {
    hotpath::FunctionsGuardBuilder::new(name)
        .percentiles(&[50, 90, 95, 99])
        .format(hotpath::Format::Table)
        .build()
}

#[cfg(not(feature = "hotpath"))]
pub fn hotpath_guard(_name: &str) {
    // no-op
}
```

### 3.2 Примеры тестов

#### kithara-worker
```rust
#[tokio::test(flavor = "current_thread")]
async fn profile_async_worker_throughput() {
    let _hotpath = crate::helpers::hotpath_guard("async_worker_throughput");

    let source = TestAsyncSource::new((0..1000).collect());
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
    assert_eq!(count, 1000);
}
```

#### kithara-decode
```rust
#[tokio::test(flavor = "current_thread")]
async fn profile_decode_resampling() {
    let _hotpath = crate::helpers::hotpath_guard("decode_resampling");

    let source = create_test_mp3_source();
    let decoder = SymphoniaDecoder::new(source).unwrap();

    let mut chunk_count = 0;
    loop {
        match decoder.next_chunk() {
            Ok(Some(_chunk)) => chunk_count += 1,
            Ok(None) => break,
            Err(e) => panic!("Decode error: {}", e),
        }
    }

    assert!(chunk_count > 0);
}
```

## 4. Интеграционные тесты

### 4.1 Новый файл `tests/tests/performance_profiling.rs`

```rust
//! Performance profiling integration tests.
//!
//! Run with: cargo test --features hotpath --test performance_profiling -- --test-threads=1

#[cfg(feature = "hotpath")]
mod profiling_tests {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn profile_hls_full_playback() {
        let _hotpath = hotpath::FunctionsGuardBuilder::new("hls_playback")
            .percentiles(&[50, 90, 95, 99])
            .build();

        // Setup HLS driver с инструментированными каналами
        // Воспроизвести несколько сегментов
        // Проверить что метрики собираются
    }

    #[tokio::test(flavor = "current_thread")]
    async fn profile_decode_pipeline() {
        let _hotpath = hotpath::FunctionsGuardBuilder::new("decode_pipeline")
            .percentiles(&[99])
            .build();

        // Тест decode pipeline с resampling
        // Измерить latency каждого этапа
    }

    #[tokio::test(flavor = "current_thread")]
    async fn profile_stream_seek_performance() {
        let _hotpath = hotpath::FunctionsGuardBuilder::new("stream_seek")
            .percentiles(&[50, 95, 99])
            .build();

        // Множественные seeks
        // Измерить время каждого seek
    }
}
```

### 4.2 CI/CD интеграция

Добавить в `.github/workflows/` (если есть):

```yaml
- name: Run performance profiling tests
  run: |
    cargo test --features hotpath --test performance_profiling -- --test-threads=1
  env:
    RUST_LOG: info
```

## 5. Бенчмарки (опционально)

Создать `benches/` директорию с Criterion бенчмарками + hotpath:

```rust
use criterion::{black_box, criterion_group, criterion_main, Criterion};

fn benchmark_worker_throughput(c: &mut Criterion) {
    c.bench_function("worker_1000_items", |b| {
        let _hotpath = hotpath_guard("worker_bench");

        b.iter(|| {
            // Benchmark logic
        });
    });
}

criterion_group!(benches, benchmark_worker_throughput);
criterion_main!(benches);
```

## 6. Документация

### 6.1 Обновить CLAUDE.md

Добавить секцию:

```markdown
### Performance profiling

Profile code with hotpath-rs:

```bash
# Run single test with profiling
cargo test --features hotpath test_name -- --test-threads=1

# Run all profiling tests
cargo test --features hotpath --test performance_profiling -- --test-threads=1

# Run application with profiling
cargo run --features hotpath --example decode_mp3
```

Memory profiling requires single-threaded runtime:

```bash
cargo test --features hotpath,hotpath-alloc -- --test-threads=1
```
```

### 6.2 Создать `docs/PROFILING.md`

Руководство по использованию hotpath в проекте:
- Когда профилировать
- Интерпретация результатов
- Best practices для async кода
- Известные ограничения (single-threaded для alloc tracking)

## 7. Этапы внедрения (roadmap)

### Phase 1: Foundation (1-2 дня)
- [ ] Добавить hotpath зависимость в workspace
- [ ] Создать feature flags в core крейтах
- [ ] Написать hotpath_helpers для тестов
- [ ] Документировать в CLAUDE.md

### Phase 2: Core Instrumentation (2-3 дня)
- [ ] Инструментировать kithara-worker (AsyncWorker, SyncWorker)
- [ ] Добавить channel monitoring
- [ ] Написать 3-5 unit тестов с профилированием
- [ ] Проверить что тесты работают с/без feature flag

### Phase 3: Domain Instrumentation (2-3 дня)
- [ ] Инструментировать kithara-decode (decoder, resampler)
- [ ] Инструментировать kithara-stream (prefetch, SyncReader)
- [ ] Инструментировать kithara-hls (segment fetch, ABR)
- [ ] Написать integration тесты

### Phase 4: Advanced & CI (1-2 дня)
- [ ] Добавить memory profiling (hotpath-alloc)
- [ ] Создать dedicated performance_profiling test suite
- [ ] Интегрировать в CI/CD (опционально)
- [ ] Написать docs/PROFILING.md

### Phase 5: Optimization (ongoing)
- [ ] Использовать профилирование для выявления узких мест
- [ ] Создать baseline метрики
- [ ] Отслеживать регрессии производительности

## 8. Потенциальные тесты

### kithara-worker
1. `profile_async_worker_throughput` - пропускная способность async worker
2. `profile_sync_worker_blocking` - overhead blocking операций
3. `profile_worker_command_latency` - задержка обработки команд
4. `profile_epoch_invalidation` - стоимость epoch invalidation
5. `profile_channel_backpressure` - поведение при заполнении каналов

### kithara-decode
1. `profile_mp3_decode_chunks` - скорость декодирования MP3
2. `profile_aac_decode_chunks` - скорость декодирования AAC
3. `profile_resampler_overhead` - overhead resampling
4. `profile_resampler_flush` - время flush resampler
5. `profile_variable_speed_playback` - производительность при переменной скорости

### kithara-stream
1. `profile_prefetch_worker_latency` - задержка prefetch
2. `profile_sync_reader_seeks` - производительность seek операций
3. `profile_range_wait_coordination` - координация wait_range
4. `profile_large_file_streaming` - стриминг больших файлов
5. `profile_chunk_size_impact` - влияние размера chunk на производительность

### kithara-hls
1. `profile_segment_fetch_parallel` - параллельная загрузка сегментов
2. `profile_variant_switch_time` - время переключения вариантов
3. `profile_abr_decision_overhead` - overhead ABR контроллера
4. `profile_playlist_parsing` - скорость парсинга плейлистов
5. `profile_key_fetch_caching` - эффективность кеширования ключей

### kithara-net
1. `profile_http_connection_reuse` - переиспользование соединений
2. `profile_retry_backoff_timing` - таймиги retry механизма
3. `profile_streaming_response_overhead` - overhead streaming responses
4. `profile_concurrent_requests` - производительность параллельных запросов
5. `profile_timeout_handling` - стоимость timeout обработки

### Integration
1. `profile_full_mp3_decode` - полное декодирование MP3 файла
2. `profile_hls_vod_playback` - воспроизведение HLS VOD
3. `profile_hls_with_seeks` - HLS с множественными seeks
4. `profile_offline_cache_hit` - производительность при cache hit
5. `profile_variant_adaptive_switching` - адаптивное переключение качества

## 9. Примечания

- **Single-threaded requirement**: Memory profiling требует `tokio::test(flavor = "current_thread")`
- **Test isolation**: Запускать с `--test-threads=1` для точных метрик
- **Zero overhead**: Когда feature выключен, код должен компилироваться без изменений
- **CI considerations**: Профилирование в CI может быть нестабильным из-за shared resources

## 10. Следующие шаги

1. Обсудить приоритеты: какие области наиболее критичны?
2. Начать с Phase 1 (Foundation)
3. Итеративно добавлять инструментацию
4. Собирать baseline метрики для будущих сравнений
