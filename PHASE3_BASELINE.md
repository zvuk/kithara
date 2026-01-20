# Phase 3: Baseline Coverage Analysis - Complete ✅

## Обзор

**Дата:** 2026-01-20
**Статус:** ✅ BASELINE MEASURED
**Инструмент:** cargo-llvm-cov 0.6.23 (native macOS)
**Команда:** `cargo llvm-cov --workspace --html --ignore-run-fail`

---

## Итоговый Baseline Coverage

```
TOTAL Coverage:
  Regions:   79.08%  (6073 покрыто / 7680 всего, 1607 пропущено)
  Functions: 74.91%  (642 выполнено / 857 всего, 215 пропущено)
  Lines:     81.80%  (4882 покрыто / 5968 всего, 1086 пропущено)
```

**Результат:** 🎉 **Baseline 81.80%** - ВЫШЕ ожидаемого (прогноз был 65-70%)!

---

## Coverage по крейтам

### 🟢 Высокий coverage (85%+ lines)

#### kithara-worker
```
Lines: 93.30%  (195/209, 14 missed)
Functions: 90.91%  (20/22, 2 missed)
Regions: 94.67%  (231/244, 13 missed)
```
**Файлы:**
- `src/lib.rs`: 93.30% - отличный coverage для async/sync worker logic

**Оценка:** ✅ Excellent

---

#### kithara-hls (ABR подсистема)
```
ABR Controller:
  Lines: 97.90%  (327/334, 7 missed)
  Functions: 100%  (24/24, 0 missed)
  Regions: 99.00%  (396/400, 4 missed)

ABR Estimator:
  Lines: 99.07%  (106/107, 1 missed)
  Functions: 100%  (12/12, 0 missed)
  Regions: 99.31%  (144/145, 1 missed)

ABR Types:
  Lines: 100%  (31/31, 0 missed)
  Functions: 100%  (5/5, 0 missed)
  Regions: 100%  (25/25, 0 missed)
```

**Оценка:** ✅✅ Outstanding - благодаря Phase 2 mockall тестам!

**Файлы:**
- `src/abr/controller.rs`: 97.90% - sequence tests + decision logic
- `src/abr/estimator.rs`: 99.07% - EWMA throughput estimation
- `src/abr/types.rs`: 100% - simple types

---

#### kithara-hls (Fetch подсистема)
```
Fetch Manager:
  Lines: 92.26%  (310/336, 26 missed)
  Functions: 98.00%  (49/50, 1 missed)
  Regions: 91.33%  (558/611, 53 missed)
```

**Оценка:** ✅✅ Outstanding - mockall unit tests с MockNet

**Файлы:**
- `src/fetch.rs`: 92.26% - fetch_playlist, fetch_key, caching logic

---

#### kithara-file
```
File Source:
  Lines: 100%  (66/66, 0 missed)
  Functions: 100%  (7/7, 0 missed)
  Regions: 98.23%  (111/113, 2 missed)

File Session:
  Lines: 89.41%  (76/85, 9 missed)
  Functions: 76.19%  (16/21, 5 missed)
  Regions: 84.44%  (76/90, 14 missed)
```

**Оценка:** ✅ Excellent для source, хорошо для session

**Файлы:**
- `src/source.rs`: 100% - progressive download, seek logic
- `src/session.rs`: 89.41% - координация download/decode

---

#### kithara-net
```
Client:
  Lines: 100%  (43/43, 0 missed)
  Functions: 100%  (12/12, 0 missed)
  Regions: 100%  (54/54, 0 missed)

Types:
  Lines: 100%  (64/64, 0 missed)
  Functions: 100%  (16/16, 0 missed)
  Regions: 100%  (72/72, 0 missed)

Timeout:
  Lines: 93.33%  (14/15, 1 missed)
  Functions: 88.89%  (8/9, 1 missed)
  Regions: 93.33%  (14/15, 1 missed)
```

**Оценка:** ✅✅ Outstanding

**Файлы:**
- `src/client.rs`: 100% - HTTP client wrapper
- `src/types.rs`: 100% - Headers, URL utils
- `src/timeout.rs`: 93.33% - timeout decorator

---

#### kithara-storage
```
Streaming:
  Lines: 90.71%  (127/140, 13 missed)
  Functions: 91.30%  (21/23, 2 missed)
  Regions: 88.48%  (146/165, 19 missed)

Atomic:
  Lines: 88.64%  (39/44, 5 missed)
  Functions: 90.00%  (9/10, 1 missed)
  Regions: 84.78%  (39/46, 7 missed)
```

**Оценка:** ✅ Excellent - comprehensive integration tests

**Файлы:**
- `src/streaming.rs`: 90.71% - streaming resource with wait_range
- `src/atomic.rs`: 88.64% - atomic resource with rename

---

#### kithara-assets (большинство файлов)
```
LRU Index:
  Lines: 99.36%  (156/157, 1 missed)
  Functions: 100%  (25/25, 0 missed)
  Regions: 96.15%  (225/234, 9 missed)

Eviction:
  Lines: 85.15%  (86/101, 15 missed)
  Functions: 95.24%  (20/21, 1 missed)
  Regions: 89.35%  (151/169, 18 missed)

Lease:
  Lines: 88.04%  (81/92, 11 missed)
  Functions: 83.33%  (20/24, 4 missed)
  Regions: 88.72%  (118/133, 15 missed)

Processing:
  Lines: 82.59%  (166/201, 35 missed)
  Functions: 65.79%  (25/38, 13 missed)
  Regions: 86.13%  (267/310, 43 missed)
```

**Оценка:** ✅ Excellent для LRU, хорошо для остальных

**Файлы:**
- `src/index/lru.rs`: 99.36% - LRU eviction logic
- `src/evict.rs`: 85.15% - EvictAssets decorator
- `src/lease.rs`: 88.04% - LeaseAssets decorator
- `src/processing.rs`: 82.59% - asset processing pipeline

---

### 🟡 Средний coverage (60-85% lines)

#### kithara-hls (некоторые модули)
```
Playlist Manager:
  Lines: 88.54%  (85/96, 11 missed)
  Functions: 73.91%  (17/23, 6 missed)
  Regions: 85.40%  (117/137, 20 missed)

Index (Segment):
  Lines: 88.40%  (160/181, 21 missed)
  Functions: 72.00%  (18/25, 7 missed)
  Regions: 88.61%  (210/237, 27 missed)

Source:
  Lines: 88.76%  (79/89, 10 missed)
  Functions: 62.50%  (5/8, 3 missed)
  Regions: 89.66%  (130/145, 15 missed)

Parsing:
  Lines: 78.62%  (125/159, 34 missed)
  Functions: 60.00%  (15/25, 10 missed)
  Regions: 72.05%  (165/229, 64 missed)

Pipeline:
  Lines: 97.80%  (534/546, 12 missed) - отлично!
  Functions: 70.00%  (14/20, 6 missed)
  Regions: 85.42%  (164/192, 28 missed)
```

**Оценка:** 🟡 Good, но parsing и некоторые functions нужно улучшить

**Файлы:**
- `src/playlist.rs`: 88.54% - playlist fetch/parse
- `src/index.rs`: 88.40% - segment index management
- `src/source.rs`: 88.76% - HLS source adapter
- `src/parsing.rs`: 78.62% - M3U8 parsing helpers
- `src/stream/pipeline.rs`: 97.80% lines (отлично!), но 70% functions

---

#### kithara-bufpool
```
Lines: 81.48%  (220/270, 50 missed)
Functions: 75.93%  (41/54, 13 missed)
Regions: 84.96%  (339/399, 60 missed)
```

**Оценка:** 🟡 Good

**Файлы:**
- `src/lib.rs`: 81.48% - buffer pool with allocation tracking

---

#### kithara-stream
```
Source:
  Lines: 82.52%  (170/206, 36 missed)
  Functions: 72.22%  (13/18, 5 missed)
  Regions: 86.74%  (229/264, 35 missed)

Stream Source:
  Lines: 76.92%  (30/39, 9 missed)
  Functions: 72.73%  (8/11, 3 missed)
  Regions: 73.17%  (30/41, 11 missed)

Pipe:
  Lines: 70.10%  (68/97, 29 missed)
  Functions: 66.67%  (6/9, 3 missed)
  Regions: 68.60%  (83/121, 38 missed)
```

**Оценка:** 🟡 Good для source, средне для pipe

**Файлы:**
- `src/source.rs`: 82.52% - byte source with seek
- `src/stream_source.rs`: 76.92% - stream adapter
- `src/pipe.rs`: 70.10% - pipe with backpressure

---

#### kithara-decode (некоторые модули)
```
Resampler:
  Lines: 83.90%  (422/503, 81 missed)
  Functions: 91.43%  (32/35, 3 missed)
  Regions: 80.69%  (635/787, 152 missed)

Source Reader:
  Lines: 82.54%  (52/63, 11 missed)
  Functions: 83.33%  (5/6, 1 missed)
  Regions: 83.53%  (71/85, 14 missed)
```

**Оценка:** 🟡 Good

**Файлы:**
- `src/resampler/processor.rs`: 83.90% - audio resampling
- `src/source_reader.rs`: 82.54% - source adapter for decoder

---

#### kithara-net (retry)
```
Retry:
  Lines: 69.49%  (41/59, 18 missed)
  Functions: 66.67%  (12/18, 6 missed)
  Regions: 63.38%  (45/71, 26 missed)

Error:
  Lines: 84.09%  (37/44, 7 missed)
  Functions: 66.67%  (6/9, 3 missed)
  Regions: 80.77%  (42/52, 10 missed)
```

**Оценка:** 🟡 Средне для retry (63%), хорошо для error

**Файлы:**
- `src/retry.rs`: 69.49% - retry decorator (нужно больше error path tests)
- `src/error.rs`: 84.09% - error types

---

#### kithara-assets (некоторые модули)
```
Base:
  Lines: 86.36%  (57/66, 9 missed)
  Functions: 84.21%  (16/19, 3 missed)
  Regions: 85.11%  (80/94, 14 missed)

Key:
  Lines: 81.82%  (63/77, 14 missed)
  Functions: 71.43%  (10/14, 4 missed)
  Regions: 84.62%  (110/130, 20 missed)

Store:
  Lines: 80.85%  (114/141, 27 missed)
  Functions: 73.33%  (22/30, 8 missed)
  Regions: 79.39%  (131/165, 34 missed)

Cache:
  Lines: 88.46%  (23/26, 3 missed)
  Functions: 80.00%  (8/10, 2 missed)
  Regions: 87.50%  (21/24, 3 missed)

Resource:
  Lines: 75.00%  (30/40, 10 missed)
  Functions: 76.92%  (10/13, 3 missed)
  Regions: 68.42%  (26/38, 12 missed)
```

**Оценка:** 🟡 Good

**Файлы:**
- `src/base.rs`: 86.36% - base trait implementations
- `src/key.rs`: 81.82% - ResourceKey with URL parsing
- `src/store.rs`: 80.85% - AssetStore facade
- `src/cache.rs`: 88.46% - CachedAssets decorator
- `src/resource.rs`: 75.00% - resource abstractions

---

### 🔴 Низкий coverage (<60% lines)

#### kithara-decode (большинство модулей)
```
PCM Source:
  Lines: 0%  (0/22, 22 missed)  ❌ COMPLETELY UNCOVERED
  Functions: 0%  (0/7, 7 missed)
  Regions: 0%  (0/23, 23 missed)

Symphonia Decoder:
  Lines: 49.83%  (150/301, 151 missed)
  Functions: 45.16%  (14/31, 17 missed)
  Regions: 44.65%  (192/430, 238 missed)

Pipeline:
  Lines: 55.24%  (174/315, 141 missed)
  Functions: 57.50%  (23/40, 17 missed)
  Regions: 49.21%  (219/445, 226 missed)

Types:
  Lines: 34.78%  (8/23, 15 missed)
  Functions: 33.33%  (2/6, 4 missed)
  Regions: 38.46%  (10/26, 16 missed)
```

**Оценка:** 🔴 Poor - КРИТИЧЕСКИЕ ПРОБЛЕМЫ

**Файлы:**
- `src/pcm_source.rs`: **0%** - НУЖНЫ ТЕСТЫ!
- `src/symphonia_mod/decoder.rs`: 49.83% - Symphonia wrapper
- `src/pipeline.rs`: 55.24% - decode pipeline
- `src/types.rs`: 34.78% - decode types

**Приоритет:** 🔥 HIGH - kithara-decode нуждается в unit-тестах!

---

#### kithara-hls (несколько модулей)
```
Adapter:
  Lines: 52.76%  (67/127, 60 missed)
  Functions: 43.48%  (10/23, 13 missed)
  Regions: 49.73%  (93/187, 94 missed)

Keys (Manager):
  Lines: 53.57%  (60/112, 52 missed)
  Functions: 50.00%  (8/16, 8 missed)
  Regions: 52.44%  (86/164, 78 missed)

Options:
  Lines: 66.10%  (39/59, 20 missed)
  Functions: 54.55%  (6/11, 5 missed)
  Regions: 56.00%  (28/50, 22 missed)
```

**Оценка:** 🔴 Poor - НУЖНЫ UNIT-ТЕСТЫ

**Файлы:**
- `src/adapter.rs`: 52.76% - HLS adapter coordination
- `src/keys.rs`: 53.57% - encryption key management
- `src/options.rs`: 66.10% - HLS configuration

**Приоритет:** 🔥 MEDIUM - важные модули, но не критичные

---

#### kithara-stream
```
Media Info:
  Lines: 53.66%  (22/41, 19 missed)
  Functions: 16.67%  (1/6, 5 missed) ❌ VERY LOW
  Regions: 57.78%  (26/45, 19 missed)
```

**Оценка:** 🔴 Poor - только 16.67% functions!

**Файлы:**
- `src/media_info.rs`: 53.66% lines, но 16.67% functions - НУЖНЫ ТЕСТЫ

**Приоритет:** 🔥 MEDIUM

---

#### kithara-file
```
Options:
  Lines: 30.30%  (10/33, 23 missed)
  Functions: 14.29%  (1/7, 6 missed) ❌ VERY LOW
  Regions: 16.67%  (5/30, 25 missed)
```

**Оценка:** 🔴 Very Poor - только 14% functions!

**Файлы:**
- `src/options.rs`: 30.30% lines, 14.29% functions - просто builder/getters?

**Приоритет:** 🟡 LOW - если это только builder patterns

---

#### kithara-assets
```
Pin Index:
  Lines: 60.87%  (28/46, 18 missed)
  Functions: 53.85%  (7/13, 6 missed)
  Regions: 51.22%  (42/82, 40 missed)
```

**Оценка:** 🔴 Poor - pin/lease logic недостаточно покрыт

**Файлы:**
- `src/index/pin.rs`: 60.87% - pin index management

**Приоритет:** 🔥 MEDIUM - важная функциональность

---

#### random-access-disk (внешняя зависимость)
```
Default:
  Lines: 47.83%  (11/23, 12 missed)
  Functions: 66.67%  (4/6, 2 missed)
  Regions: 40.74%  (11/27, 16 missed)

Lib:
  Lines: 77.78%  (56/72, 16 missed)
  Functions: 68.75%  (11/16, 5 missed)
  Regions: 75.61%  (62/82, 20 missed)
```

**Оценка:** 🟡 Средне, но это внешняя зависимость

**Приоритет:** 🟢 LOW - не наша кодовая база

---

## Сравнение с прогнозом (из CODECOV_INTEGRATION.md)

### Прогноз vs Факт

| Модуль | Прогноз | Факт (Lines) | Статус |
|--------|---------|--------------|--------|
| kithara-bufpool | ~85% | 81.48% | ✅ Близко |
| kithara-net | ~80% | 84.09-100% | ✅✅ Лучше |
| kithara-hls (ABR) | ~75% | 97-100% | ✅✅✅ ОТЛИЧНО |
| kithara-hls (fetch) | ~75% | 92.26% | ✅✅ Отлично |
| kithara-worker | ~70% | 93.30% | ✅✅ Отлично |
| kithara-hls (общий) | ~70% | 52-100% | 🟡 Разброс |
| kithara-stream | ~65% | 53-82% | 🟡 Близко |
| kithara-file | ~60% | 30-100% | 🟡 Разброс |
| kithara-assets | ~60% | 51-99% | 🟡 Разброс |
| kithara-storage | ~55% | 88-90% | ✅✅ ЛУЧШЕ |
| kithara-decode | N/A | 0-83% | 🔴 Проблемы |

### Почему baseline ЛУЧШЕ прогноза?

1. **Phase 2 mockall тесты** добавили +15-20% к ABR и FetchManager
2. **Хорошие integration тесты** для storage, net, worker
3. **Недооценка** существующих unit-тестов в Phase 1

---

## Uncovered Areas (Критические)

### 🔥 Priority 1: MUST FIX

#### 1. kithara-decode/src/pcm_source.rs - 0% coverage
```
Status: COMPLETELY UNCOVERED
Lines: 0/22 (22 missed)
Functions: 0/7 (7 missed)
```

**Действия:**
- Добавить unit-тесты для PcmSource
- Проверить read, seek, metadata operations
- Mock decoder для изоляции

**Impact:** 🔥 CRITICAL - публичный API не протестирован

---

#### 2. kithara-decode/src/symphonia_mod/decoder.rs - 44.65%
```
Lines: 150/301 (151 missed)
Functions: 14/31 (17 missed)
```

**Uncovered:**
- Error handling paths (decoder initialization failures)
- Edge cases в packet processing
- Format detection logic

**Действия:**
- Unit-тесты с mock MediaSource
- Error injection tests
- Codec-specific tests (AAC, MP3, FLAC)

**Impact:** 🔥 HIGH - core decoding logic

---

#### 3. kithara-decode/src/pipeline.rs - 55.24%
```
Lines: 174/315 (141 missed)
Functions: 23/40 (17 missed)
```

**Uncovered:**
- Pipeline error recovery
- Resampling edge cases
- Buffer management

**Действия:**
- Unit-тесты для каждого pipeline stage
- Error propagation tests
- Resource cleanup tests

**Impact:** 🔥 HIGH - decode orchestration

---

### 🔥 Priority 2: Should Fix

#### 4. kithara-hls/src/adapter.rs - 52.76%
```
Lines: 67/127 (60 missed)
Functions: 10/23 (13 missed)
```

**Uncovered:**
- HLS orchestration edge cases
- State machine transitions
- Error handling

**Действия:**
- Unit-тесты с mock components
- State transition tests
- Integration tests для полных flows

**Impact:** 🔥 MEDIUM - важная интеграция

---

#### 5. kithara-hls/src/keys.rs - 53.57%
```
Lines: 60/112 (52 missed)
Functions: 8/16 (8 missed)
```

**Uncovered:**
- Key fetching error paths
- Caching logic edge cases
- Encryption key rotation

**Действия:**
- Unit-тесты с MockNet
- Cache hit/miss tests
- Error handling tests

**Impact:** 🔥 MEDIUM - encryption критично

---

#### 6. kithara-assets/src/index/pin.rs - 60.87%
```
Lines: 28/46 (18 missed)
Functions: 7/13 (6 missed)
```

**Uncovered:**
- Pin/unpin edge cases
- Concurrent pinning scenarios
- Index corruption recovery

**Действия:**
- Unit-тесты для pin operations
- Concurrent access tests
- Snapshot persistence tests

**Impact:** 🔥 MEDIUM - важная функциональность

---

#### 7. kithara-stream/src/media_info.rs - 53.66% (16.67% functions!)
```
Lines: 22/41 (19 missed)
Functions: 1/6 (5 missed) ❌
```

**Uncovered:**
- Codec parsing variations
- MediaInfo construction
- Format detection

**Действия:**
- Параметризованные тесты для всех codecs
- rstest с разными форматами
- Edge cases для invalid data

**Impact:** 🔥 MEDIUM - публичный API

---

### 🟡 Priority 3: Nice to Have

#### 8. kithara-file/src/options.rs - 30.30%
```
Lines: 10/33 (23 missed)
Functions: 1/7 (6 missed)
```

**Оценка:** Возможно просто builder patterns (низкий приоритет)

---

#### 9. kithara-net/src/retry.rs - 69.49%
```
Lines: 41/59 (18 missed)
Functions: 12/18 (6 missed)
```

**Uncovered:**
- Retry backoff edge cases
- Max retries scenarios
- Specific error types

**Действия:**
- Error injection tests
- Backoff timing tests
- Exhaustion scenarios

**Impact:** 🟡 MEDIUM - важно но не критично

---

## Quick Wins для Phase 4

### Цель: 81% → 85% (+4%)

#### 1. kithara-decode/src/pcm_source.rs (+0.4%)
**Усилия:** 2 часа
**Тесты:** 5-7 unit tests
**Coverage gain:** 0% → 100% (22 lines)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pcm_source_read() { /* ... */ }

    #[test]
    fn test_pcm_source_seek() { /* ... */ }

    #[test]
    fn test_pcm_source_metadata() { /* ... */ }
}
```

---

#### 2. kithara-stream/src/media_info.rs (+0.3%)
**Усилия:** 1 час
**Тесты:** 10-15 rstest cases
**Coverage gain:** 53% → 90% (19 lines)

```rust
#[rstest]
#[case("mp4a.40.2", Codec::AacLc)]
#[case("mp4a.40.5", Codec::AacHe)]
#[case("mp3", Codec::Mp3)]
fn test_codec_parsing(#[case] codec_str: &str, #[case] expected: Codec) {
    // ...
}
```

---

#### 3. kithara-file/src/options.rs (+0.4%)
**Усилия:** 1 час
**Тесты:** 3-5 builder tests
**Coverage gain:** 30% → 80% (23 lines)

```rust
#[test]
fn test_file_options_builder() {
    let opts = FileOptions::builder()
        .buffer_size(8192)
        .prefetch(true)
        .build();
    assert_eq!(opts.buffer_size(), 8192);
}
```

---

#### 4. kithara-decode/src/types.rs (+0.2%)
**Усилия:** 30 минут
**Тесты:** 3-5 type tests
**Coverage gain:** 34% → 80% (15 lines)

---

#### 5. kithara-assets/src/index/pin.rs (+0.3%)
**Усилия:** 2 часа
**Тесты:** 5-7 pin/unpin tests
**Coverage gain:** 60% → 85% (18 lines)

```rust
#[tokio::test]
async fn test_pin_prevents_eviction() {
    let store = /* ... */;
    let guard = store.pin("asset").await.unwrap();
    // Verify asset not evicted
}
```

---

**Итого Quick Wins:**
- **Усилия:** ~6.5 часов
- **Тесты:** ~30 новых unit tests
- **Coverage gain:** +1.6% (81.8% → 83.4%)

---

## Long-term Improvements для Phase 5

### Цель: 85% → 90%+

#### 1. kithara-decode полный рефакторинг
**Усилия:** 2-3 дня
**Тесты:** 50+ unit tests
**Coverage gain:** +3-4%

**Файлы:**
- `src/symphonia_mod/decoder.rs`: 44% → 80%
- `src/pipeline.rs`: 55% → 80%
- `src/pcm_source.rs`: 0% → 100%
- `src/types.rs`: 34% → 80%

**Подход:**
- Mock Symphonia для изоляции
- Property-based tests для pipeline
- Error injection tests
- Codec-specific tests

---

#### 2. kithara-hls недопокрытые модули
**Усилия:** 1-2 дня
**Тесты:** 30+ unit tests
**Coverage gain:** +2-3%

**Файлы:**
- `src/adapter.rs`: 52% → 75%
- `src/keys.rs`: 53% → 75%
- `src/options.rs`: 66% → 80%
- `src/parsing.rs`: 78% → 85%

**Подход:**
- Unit-тесты с mocks
- State machine tests для adapter
- Encryption edge cases для keys

---

#### 3. Error path coverage
**Усилия:** 1 день
**Тесты:** 20+ error tests
**Coverage gain:** +1-2%

**Модули:**
- kithara-net retry logic
- kithara-hls error propagation
- kithara-decode failure recovery

**Подход:**
- Error injection
- Fault simulation
- Resource exhaustion

---

#### 4. Property-based testing
**Усилия:** 2 дня
**Тесты:** 10-15 proptest cases
**Coverage gain:** +1-2%

**Целевые модули:**
- ABR hysteresis logic
- LRU eviction invariants
- Seek operations edge cases
- Resampler accuracy

```rust
use proptest::prelude::*;

proptest! {
    #[test]
    fn abr_never_exceeds_throughput(
        throughput in 100_000u64..10_000_000u64,
        buffer in 0.0f64..60.0f64
    ) {
        let selected_bitrate = controller.select(throughput, buffer);
        assert!(selected_bitrate <= throughput * safety_factor);
    }
}
```

---

## HTML Coverage Report

**Location:** `/Users/litvinenko-pv/code/kithara/target/llvm-cov/html/index.html`

**Содержание:**
- Интерактивная таблица по файлам
- Drill-down в каждый файл с подсветкой
- Uncovered lines выделены красным
- Частично покрытые branches выделены желтым

**Использование:**
```bash
open target/llvm-cov/html/index.html
```

---

## LCOV Coverage для Codecov

**Генерация LCOV:**
```bash
cargo llvm-cov --workspace --lcov --output-path target/coverage/lcov.info
```

**Файл:** `target/coverage/lcov.info`

**Использование:**
- Загрузка в Codecov.io через GitHub Actions
- Локальный просмотр через genhtml (если установлен)
- Интеграция с IDE (VSCode, IntelliJ)

---

## Следующие шаги

### Phase 3 ✅ COMPLETE

- ✅ Phase 3.1: Infrastructure проверена
- ✅ Phase 3.2: Local coverage измерен (81.80%)
- ✅ Phase 3.3: Baseline анализ завершен
- ✅ Phase 3.4: Документация создана (этот файл)

### Phase 4: Coverage Improvements (Optional)

**Цель:** 81.8% → 85%+

**Приоритеты:**
1. kithara-decode: pcm_source.rs unit tests (+0.4%)
2. kithara-stream: media_info.rs rstest (+0.3%)
3. kithara-file: options.rs builder tests (+0.4%)
4. kithara-assets: pin.rs pin/unpin tests (+0.3%)
5. kithara-decode: types.rs type tests (+0.2%)

**Общие усилия:** ~6.5 часов для +1.6%

---

## Выводы

### Что сделано хорошо ✅

1. **ABR подсистема:** 97-100% благодаря Phase 2 mockall tests
2. **FetchManager:** 92% благодаря MockNet unit tests
3. **kithara-storage:** 88-90% благодаря comprehensive tests
4. **kithara-net:** 84-100% благодаря хорошим unit tests
5. **kithara-worker:** 93% благодаря async/sync tests

### Что нужно улучшить 🔴

1. **kithara-decode:** КРИТИЧНО - 0-55% coverage
   - pcm_source.rs: 0% (не покрыт вообще!)
   - decoder.rs: 44%
   - pipeline.rs: 55%
   - types.rs: 34%

2. **kithara-hls некоторые модули:** 52-66%
   - adapter.rs: 52%
   - keys.rs: 53%
   - options.rs: 66%

3. **kithara-assets pin index:** 60%

4. **kithara-stream media_info:** 53% (16% functions!)

### Рекомендации

**Немедленные действия:**
1. Добавить unit-тесты для kithara-decode/pcm_source.rs (0% → 100%)
2. Параметризовать media_info.rs codec tests (16% functions → 80%+)
3. Добавить builder tests для file options (30% → 80%)

**Краткосрочные (1-2 недели):**
1. Unit-тесты для kithara-decode decoder/pipeline (44-55% → 75%+)
2. Mock-based tests для kithara-hls adapter/keys (52-53% → 75%+)
3. Pin/unpin tests для assets pin index (60% → 80%+)

**Долгосрочные (1 месяц):**
1. Property-based tests для ABR/LRU/seek operations
2. Error path coverage для всех модулей
3. Integration → unit migration где возможно

---

**Итого:** Baseline 81.80% ОТЛИЧНО для старта, но kithara-decode требует срочного внимания!

**Phase 3 Status: ✅ COMPLETE**
