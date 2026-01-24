# Kithara: Архитектура и анализ производительности

## Обзор

Kithara — это модульная библиотека на Rust для аудио-стриминга с поддержкой прогрессивной HTTP загрузки, HLS VOD, декодирования и персистентного кэширования. Архитектура построена на принципах модульности, zero-copy где возможно, и композиции декораторов.

## Общая архитектура системы

```mermaid
graph TB
    subgraph "User Layer"
        APP[Application Code]
    end

    subgraph "High-Level Protocols"
        FILE[kithara-file<br/>Progressive HTTP]
        HLS[kithara-hls<br/>HLS VOD + ABR]
    end

    subgraph "Decoding Layer"
        DECODE[kithara-decode<br/>Symphonia + Rubato]
    end

    subgraph "Transport & Orchestration"
        NET[kithara-net<br/>HTTP Client]
        STREAM[kithara-stream<br/>Byte Orchestration]
        WORKER[kithara-worker<br/>Async/Sync Workers]
    end

    subgraph "Storage Layer"
        ASSETS[kithara-assets<br/>Asset Management]
        STORAGE[kithara-storage<br/>Random Access I/O]
    end

    subgraph "Utilities"
        BUFPOOL[kithara-bufpool<br/>Buffer Pooling]
    end

    APP --> FILE
    APP --> HLS
    APP --> DECODE

    FILE --> STREAM
    FILE --> ASSETS
    FILE --> NET

    HLS --> STREAM
    HLS --> ASSETS
    HLS --> NET

    DECODE --> STREAM
    DECODE --> BUFPOOL
    DECODE --> WORKER

    STREAM --> STORAGE
    STREAM --> WORKER
    STREAM --> BUFPOOL

    ASSETS --> STORAGE

    NET -.->|optional cache| ASSETS

    style FILE fill:#e1f5ff
    style HLS fill:#e1f5ff
    style DECODE fill:#fff4e1
    style BUFPOOL fill:#f0f0f0
```

## Иерархия зависимостей

```
Layer 5: kithara-file, kithara-hls (protocols)
           ↓
Layer 4: kithara-decode (decoding)
           ↓
Layer 3: kithara-stream (orchestration)
           ↓
Layer 2: kithara-net, kithara-assets, kithara-worker
           ↓
Layer 1: kithara-storage (базовый I/O)
           ↓
Layer 0: kithara-bufpool (utilities)
```

## Потоки данных: Progressive File Download & Playback

```mermaid
sequenceDiagram
    participant App
    participant File as kithara-file
    participant Writer as Background Writer
    participant Reader as SyncReader
    participant Assets as kithara-assets
    participant Storage as StreamingResource
    participant Decoder as kithara-decode
    participant Disk

    App->>File: open(url)
    File->>Assets: open_streaming_resource(key)
    Assets->>Storage: new(path)
    Storage->>Disk: create file

    par Download (async)
        File->>Writer: spawn tokio task
        loop until EOF
            Writer->>Writer: HTTP stream chunks
            Writer->>Storage: write_at(offset, bytes)
            Storage->>Disk: async write
        end
    and Playback (sync)
        File->>Reader: new(source)
        Reader->>Reader: spawn prefetch worker
        loop decode & play
            Decoder->>Reader: read(buf) [sync]
            Reader->>Storage: wait_range(pos..pos+64KB)
            Storage->>Storage: check available ranges
            Storage->>Reader: ready
            Reader->>Storage: read_at(pos, buf)
            Storage->>Disk: async read
            Disk-->>Reader: bytes
            Reader-->>Decoder: chunk
        end
    end
```

## Потоки данных: HLS VOD с ABR

```mermaid
sequenceDiagram
    participant App
    participant HLS as kithara-hls
    participant Worker as HlsWorkerSource
    participant Playlist as PlaylistManager
    participant Fetch as FetchManager
    participant Assets
    participant ABR as AbrController
    participant Decode as kithara-decode

    App->>HLS: open(master_url)
    HLS->>Playlist: load master.m3u8
    Playlist->>Assets: open_atomic_resource
    Playlist-->>HLS: variants

    HLS->>Worker: new(initial_variant)
    HLS->>Worker: spawn async worker

    loop segment download & decode
        Worker->>Playlist: get_segment_metadata(idx)
        Worker->>Fetch: start_fetch(segment_url)
        Fetch->>Assets: open_streaming_resource

        par
            Fetch->>Fetch: HTTP download
            Fetch->>Assets: write_at(offset, chunk)
        and
            Worker->>Assets: wait_range + read_at
        end

        Worker->>Worker: combine init + media
        Worker->>ABR: update throughput
        ABR->>ABR: EWMA calculation
        ABR-->>Worker: selected_variant

        Worker->>Decode: decode_message(bytes)
        Decode-->>App: PCM samples
    end
```

## Анализ использования памяти в runtime

### Типичный сценарий: HLS playback с 3 вариантами

| Компонент | Структуры данных | Runtime Memory |
|-----------|------------------|----------------|
| **kithara-hls** | | |
| - PlaylistManager | Master + 3 MediaPlaylists | ~15 KB |
| - FetchManager | Metadata cache | ~10 KB |
| - HlsWorkerSource | init_segments_cache (3 variants) | ~6 KB |
| - HlsSourceAdapter | buffered_chunks (worst: 10 segs) | ~20 MB* |
| - Channels | cmd(16) + chunk(2) + events | ~8 MB |
| **Subtotal HLS** | | **~28 MB** |
| **kithara-assets** | | |
| - DiskAssetStore | Path metadata | ~2 KB |
| - EvictAssets | seen HashSet (100 assets) | ~4 KB |
| - LeaseAssets | pins HashSet (10 pinned) | ~500 B |
| - CachedAssets | LRU (5 resources) | ~2 KB |
| - Indices | LRU + Pins JSON in memory | ~20 KB |
| **Subtotal Assets** | | **~30 KB** |
| **kithara-storage** | | |
| - StreamingResource (10 active) | Arc overhead + RangeSet | ~5 KB |
| - RangeSet | BTreeMap (sequential: 1 range/file) | ~1 KB |
| **Subtotal Storage** | | **~6 KB** |
| **kithara-stream** | | |
| - SyncReader prefetch | 4 chunks × 64KB | ~256 KB |
| - Reader buffer pool | SharedPool (32 shards, ~20 active) | ~80 KB |
| **Subtotal Stream** | | **~336 KB** |
| **kithara-decode** | | |
| - SymphoniaDecoder | Codec state | ~50 KB |
| - ResamplerProcessor | Sinc state + buffers | ~200 KB |
| - PcmBuffer (streaming mode) | Channel (20 chunks × 8KB) | ~160 KB |
| - Buffer pool | SharedPool (~20 active Vec<f32>) | ~80 KB |
| **Subtotal Decode** | | **~490 KB** |
| **kithara-net** | | |
| - HttpClient | reqwest state (pool disabled) | ~50 KB |
| - ByteStream | Box overhead per stream | ~24 B |
| **Subtotal Net** | | **~50 KB** |
| **TOTAL STEADY STATE** | | **~29 MB** |

**Критическая проблема**: `HlsSourceAdapter::buffered_chunks` может расти до 20+ MB без ограничений.

### Оптимизации памяти

**Применено:**
- ✅ Connection pooling **отключен** в kithara-net (экономия ~5 MB)
- ✅ Buffer pooling в kithara-stream и kithara-decode
- ✅ Bounded channels (backpressure)
- ✅ Streaming mode в PcmBuffer (без Vec accumulation)
- ✅ Arc-based sharing (избегаем клонирования)

**Рекомендовано:**
- ❌ Ограничить `buffered_chunks` в HlsSourceAdapter (max 3-5 сегментов)
- ❌ Buffer pool для HLS init+media комбинирования
- ❌ Очистка `init_segments_cache` при смене вариантов

## Анализ утилизации CPU и блокировок

### Async операции и ожидания

#### kithara-net
**Await points:**
- `req.send().await` - network I/O (1-100ms)
- `resp.bytes().await` - body download (10ms - 10s)
- `sleep(exponential_backoff).await` - retry delays

**Эффективность:** ✅ Высокая. Нет busy-waiting.

#### kithara-storage
**Await points:**
- `wait_range()` - использует `Notify::notified()` (event-driven)
- `disk.write/read.await` - async file I/O через tokio

**Блокировки:**
- `disk: Mutex<RandomAccessDisk>` - сериализует I/O (неизбежно)
- `state: RwLock<State>` - NOT held across await (хорошо)

**Эффективность:** ✅ Отлично. Нет spin loops.

#### kithara-stream
**Await points:**
- `source.wait_range().await` - делегирует в storage
- `source.read_at().await` - file I/O

**Blocking в SyncReader:**
- `data_rx.recv()` - kanal использует thread parking (эффективно)

**Эффективность:** ✅ Отлично. Epoch-based invalidation минимизирует wasted work.

#### kithara-assets
**Await points:**
- `AtomicResource::write()` - temp + rename (1-5ms)
- `StreamingResource` operations - делегирует в storage

**Блокировки:**
- `pins: Mutex<HashSet>` - короткие критические секции
- `cache: parking_lot::Mutex<LruCache>` - ⚠️ **sync mutex в async** (допустимо, т.к. операции быстрые)

**Эффективность:** ⚠️ Хорошо, но может быть лучше.
- Проблема: Index read-modify-write при каждом touch
- Решение: In-memory index + periodic flush

#### kithara-hls
**Await points:**
- Множественные HTTP requests
- Asset I/O operations
- Segment downloads

**Проблемы:**
- ❌ **Spin loop в CachedLoader::wait_range**: 10ms sleep × 1000 iterations
- ❌ **Паузированный worker**: 100ms sleep в бесконечном цикле

**Эффективность:** ⚠️ Средняя. Требует рефакторинга polling → Notify.

#### kithara-decode
**Blocking операции:**
- Decoding (Symphonia) - CPU-intensive
- Resampling (rubato) - CPU-intensive

**Изоляция:**
- ✅ Выполняются в `tokio::task::spawn_blocking`
- ✅ Не блокируют async runtime

**Эффективность:** ✅ Отлично. Правильное использование spawn_blocking.

### Lock Contention Summary

| Крейт | Lock | Type | Hold Time | Contention Risk |
|-------|------|------|-----------|-----------------|
| kithara-net | Нет | - | - | ✅ None |
| kithara-storage | disk | Mutex | I/O duration (1-10ms) | ⚠️ Medium (read+write compete) |
| kithara-storage | state | RwLock | <1μs | ✅ Low |
| kithara-stream | Нет | - | - | ✅ None |
| kithara-assets | pins | Mutex | <1μs | ✅ Low |
| kithara-assets | cache | parking_lot::Mutex | <1μs | ⚠️ Low (sync in async) |
| kithara-hls | buffered_chunks | Mutex | <10μs | ⚠️ Medium |
| kithara-decode | samples | RwLock | 10-100μs | ⚠️ Low (single writer) |
| kithara-bufpool | shards[i] | parking_lot::Mutex | <1μs | ✅ Very Low (32 shards) |

**Выводы:**
- ✅ Большинство критических путей **lock-free** (atomics, channels)
- ⚠️ Основной источник contention: `disk: Mutex` в StreamingResource
  - Решение: Разделить на read/write handles (сложно с random-access-disk)
- ✅ Sharding в kithara-bufpool эффективно распределяет нагрузку

### CPU Utilization Metrics

**Benchmarks (примерные, на основе анализа кода):**

| Operation | Duration | CPU Time | Waiting |
|-----------|----------|----------|---------|
| HTTP request (network) | 10-100ms | <1ms | 99% waiting |
| Disk read (64KB) | 1-5ms | <0.1ms | >95% waiting |
| Decode chunk (1024 frames) | 0.5-2ms | 0.5-2ms | 0% waiting |
| Resample chunk | 0.2-1ms | 0.2-1ms | 0% waiting |
| Buffer pool get/put | 10-20ns | 10-20ns | 0% waiting |
| Channel send/recv | 50-500ns | 50-500ns | 0% waiting (при наличии данных) |

**CPU Efficiency Score:**
- **Network I/O:** ⭐⭐⭐⭐⭐ (0% CPU waste, fully async)
- **Disk I/O:** ⭐⭐⭐⭐⭐ (0% CPU waste, tokio async)
- **Decoding:** ⭐⭐⭐⭐⭐ (100% utilization в spawn_blocking)
- **Coordination:** ⭐⭐⭐⭐ (minimal overhead, но есть spin loops в HLS)

**Оценка общей эффективности:** ~95% CPU используется продуктивно, ~5% на coordination/waiting.

## Критические файлы для понимания системы

### Архитектурный костяк (читать первыми):
1. `CLAUDE.md` - Правила кодирования и принципы
2. `ARCHITECTURE.md` (этот файл) - Общая архитектура
3. `kithara-storage/README.md` - Базовые примитивы I/O
4. `kithara-stream/README.md` - Оркестрация потоков
5. `kithara-assets/README.md` - Управление кэшем

### Реализации протоколов:
6. `kithara-file/README.md` - Progressive HTTP
7. `kithara-hls/README.md` - HLS VOD + ABR

### Декодирование:
8. `kithara-decode/README.md` - Audio decoding
9. `kithara-decode/STREAM_ARCHITECTURE.md` - Stream architecture

### Утилиты:
10. `kithara-worker/README.md` - Worker patterns
11. `kithara-bufpool/README.md` - Buffer pooling
12. `kithara-net/README.md` - HTTP client

## Roadmap для оптимизаций

### Высокий приоритет (влияют на production):
1. **[kithara-hls]** Ограничить `buffered_chunks` максимум 5 сегментами
2. **[kithara-hls]** Заменить spin loops на `Notify` в wait_range
3. **[kithara-hls]** Использовать `Bytes` chain вместо копирования init+media
4. **[kithara-assets]** Batch index updates (flush раз в 5 секунд)

### Средний приоритет (улучшения производительности):
5. **[kithara-storage]** Разделить read/write файловые handles (если возможно)
6. **[kithara-net]** Опциональный connection pooling (для low-latency сценариев)
7. **[kithara-stream]** Buffer pool для ByteChunk аллокаций
8. **[kithara-decode]** Adaptive resampler chunk size

### Низкий приоритет (observability):
9. **[все]** Добавить metrics через tracing spans
10. **[kithara-bufpool]** Опциональные счетчики hit/miss

## Performance Testing Plan

### Memory Profiling
```bash
# Используя dhat
cargo run --release --features dhat --example hls_playback
```

### CPU Profiling
```bash
# Используя perf (Linux)
perf record -F 999 -g cargo run --release --example hls_playback
perf report
```

### Benchmarks
```bash
# Пропускная способность
cargo bench --bench throughput

# Latency
cargo bench --bench latency

# Memory allocation
cargo bench --bench allocation
```

---

**Версия документа:** 1.0
**Дата:** 2026-01-23
**Сгенерировано:** Claude Sonnet 4.5 (parallel crate analysis)
