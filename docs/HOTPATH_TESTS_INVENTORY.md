# Инвентаризация потенциальных тестов с hotpath-rs

## Сводка по крейтам

| Крейт | Unit Tests | Integration Tests | Приоритет |
|-------|-----------|-------------------|-----------|
| kithara-worker | 5 | 2 | Высокий |
| kithara-decode | 5 | 3 | Высокий |
| kithara-stream | 5 | 2 | Высокий |
| kithara-hls | 5 | 3 | Средний |
| kithara-net | 5 | 1 | Средний |
| **Всего** | **25** | **11** | - |

## Детальный список тестов

### 🔴 ВЫСОКИЙ ПРИОРИТЕТ

#### kithara-worker (5 unit + 2 integration)

**Unit тесты:**
1. ✅ `profile_async_worker_throughput`
   - Метрика: items/sec через async worker
   - Baseline target: >10k items/sec

2. ✅ `profile_sync_worker_blocking`
   - Метрика: overhead blocking в spawn_blocking
   - Baseline target: <100µs overhead per chunk

3. ✅ `profile_worker_command_latency`
   - Метрика: время от send команды до обработки
   - Baseline target: <1ms для seek команды

4. ✅ `profile_epoch_invalidation`
   - Метрика: стоимость invalidation при seek
   - Baseline target: <500µs для полной invalidation

5. ✅ `profile_channel_backpressure`
   - Метрика: поведение при заполненном канале
   - Baseline target: graceful degradation без deadlock

**Integration тесты:**
1. ✅ `profile_worker_with_real_decoder`
   - Полный цикл: byte source → worker → chunks
   - Метрика: end-to-end latency

2. ✅ `profile_worker_seek_invalidation`
   - Multiple seeks во время воспроизведения
   - Метрика: recovery time после seek

---

#### kithara-decode (5 unit + 3 integration)

**Unit тесты:**
1. ✅ `profile_mp3_decode_chunks`
   - Метрика: chunks/sec для MP3
   - Baseline target: >500 chunks/sec (зависит от размера)

2. ✅ `profile_aac_decode_chunks`
   - Метрика: chunks/sec для AAC
   - Baseline target: >400 chunks/sec

3. ✅ `profile_resampler_overhead`
   - Метрика: overhead resampling vs passthrough
   - Baseline target: <20% overhead для 44.1→48kHz

4. ✅ `profile_resampler_flush`
   - Метрика: время финального flush
   - Baseline target: <10ms

5. ✅ `profile_variable_speed_playback`
   - Метрика: overhead при 0.5x, 1.0x, 2.0x скорости
   - Baseline target: <30% overhead для 2x speed

**Integration тесты:**
1. ✅ `profile_full_mp3_decode`
   - Декодирование полного MP3 файла (3-5 минут)
   - Метрика: real-time factor (должен быть <<1.0)

2. ✅ `profile_decode_with_seeks`
   - Декодирование с 10 random seeks
   - Метрика: seek latency + recovery time

3. ✅ `profile_decode_pipeline_memory`
   - С флагом hotpath-alloc
   - Метрика: allocations per chunk

---

#### kithara-stream (5 unit + 2 integration)

**Unit тесты:**
1. ✅ `profile_prefetch_worker_latency`
   - Метрика: время от запроса до получения chunk
   - Baseline target: <5ms при cache hit

2. ✅ `profile_sync_reader_seeks`
   - Метрика: latency для forward/backward seeks
   - Baseline target: <20ms для forward, <50ms для backward

3. ✅ `profile_range_wait_coordination`
   - Метрика: время wait_range при разных сценариях
   - Baseline target: instant для готовых данных

4. ✅ `profile_large_file_streaming`
   - Метрика: throughput для файла >100MB
   - Baseline target: >50MB/sec read speed

5. ✅ `profile_chunk_size_impact`
   - Метрика: влияние размера chunk (4KB vs 64KB vs 1MB)
   - Baseline target: найти оптимальный размер

**Integration тесты:**
1. ✅ `profile_stream_progressive_download`
   - HTTP source с прогрессивной загрузкой
   - Метрика: read latency vs download progress

2. ✅ `profile_stream_offline_playback`
   - Полностью закешированный файл
   - Метрика: minimal latency baseline

---

### 🟡 СРЕДНИЙ ПРИОРИТЕТ

#### kithara-hls (5 unit + 3 integration)

**Unit тесты:**
1. ⚪ `profile_segment_fetch_parallel`
   - Метрика: throughput при параллельной загрузке
   - Baseline target: >3 сегмента параллельно

2. ⚪ `profile_variant_switch_time`
   - Метрика: время от решения до начала загрузки нового варианта
   - Baseline target: <100ms

3. ⚪ `profile_abr_decision_overhead`
   - Метрика: CPU time для ABR решения
   - Baseline target: <1ms per decision

4. ⚪ `profile_playlist_parsing`
   - Метрика: время парсинга master + variant playlists
   - Baseline target: <10ms для типичного VOD

5. ⚪ `profile_key_fetch_caching`
   - Метрика: cache hit/miss latency
   - Baseline target: <1ms для hit, <50ms для miss

**Integration тесты:**
1. ⚪ `profile_hls_vod_playback`
   - Воспроизведение 10 сегментов VOD
   - Метрика: startup latency + steady-state throughput

2. ⚪ `profile_hls_adaptive_switching`
   - Автоматическое переключение вариантов (3-4 switches)
   - Метрика: seamlessness (gap между вариантами)

3. ⚪ `profile_hls_offline_mode`
   - Полностью закешированный VOD
   - Метрика: minimal overhead baseline

---

#### kithara-net (5 unit + 1 integration)

**Unit тесты:**
1. ⚪ `profile_http_connection_reuse`
   - Метрика: latency для reused vs new connection
   - Baseline target: >50% reduction для reused

2. ⚪ `profile_retry_backoff_timing`
   - Метрика: реальное время между retries
   - Baseline target: соответствие экспоненциальному backoff

3. ⚪ `profile_streaming_response_overhead`
   - Метрика: overhead streaming API vs blocking
   - Baseline target: <5% overhead

4. ⚪ `profile_concurrent_requests`
   - Метрика: throughput при N параллельных запросах
   - Baseline target: linear scaling до N=10

5. ⚪ `profile_timeout_handling`
   - Метрика: overhead timeout мониторинга
   - Baseline target: <1% CPU overhead

**Integration тесты:**
1. ⚪ `profile_net_under_load`
   - 50 параллельных запросов с разными размерами
   - Метрика: P99 latency должен быть <500ms

---

## Тестовые данные и fixtures

### Требования к тестовым данным:

1. **Аудио файлы:**
   - MP3: 128kbps, 44.1kHz, stereo, 30 sec (~480KB)
   - MP3: 320kbps, 48kHz, stereo, 3 min (~7MB)
   - AAC: 128kbps, 44.1kHz, stereo, 30 sec (~480KB)

2. **HLS плейлисты:**
   - VOD с 3 вариантами (480p, 720p, 1080p)
   - 10 сегментов по 6 секунд
   - С AES-128 encryption

3. **Mock HTTP server:**
   - Контролируемая latency (10ms, 50ms, 100ms, 500ms)
   - Контролируемая throughput (1Mbps, 5Mbps, 10Mbps)
   - Возможность failure injection

### Размещение:

```
tests/
  fixtures/
    audio/
      test_30s_128kbps.mp3
      test_3min_320kbps.mp3
      test_30s_aac.m4a
    hls/
      vod_multi_variant/
        master.m3u8
        variant_480p.m3u8
        variant_720p.m3u8
        variant_1080p.m3u8
        seg_*.ts
        key.bin
  helpers/
    hotpath_helpers.rs
    mock_server.rs
    test_data_generator.rs
```

## Приоритизация

### Фаза 1: Critical Path (week 1)
- kithara-worker: все unit тесты
- kithara-decode: MP3 decode тесты
- kithara-stream: prefetch и seek тесты

### Фаза 2: Core Features (week 2)
- kithara-decode: AAC + resampler тесты
- kithara-stream: оставшиеся тесты
- Integration: decode pipeline

### Фаза 3: Network & HLS (week 3)
- kithara-hls: unit тесты
- kithara-net: unit тесты
- Integration: HLS playback

### Фаза 4: Advanced & Memory (week 4)
- Integration тесты со сложными сценариями
- Memory profiling (hotpath-alloc)
- Performance regression suite для CI

## Baseline метрики

После написания тестов, создать файл `docs/PERFORMANCE_BASELINES.md` с результатами:

```markdown
# Performance Baselines

Generated: 2026-01-20
Machine: MacBook Pro M1, 16GB RAM
Rust: 1.85

## kithara-worker
- async_worker_throughput: 15,234 items/sec (p99: 85µs)
- sync_worker_blocking: 68µs overhead per chunk
...

## kithara-decode
- mp3_decode_chunks: 623 chunks/sec (p99: 2.1ms)
...
```

Это позволит отслеживать регрессии при изменениях кода.

## Команды для запуска

```bash
# Все профилирующие тесты
cargo test --features hotpath --test performance_profiling -- --test-threads=1

# Конкретный крейт
cargo test --features hotpath -p kithara-worker -- --test-threads=1

# С memory profiling
cargo test --features hotpath,hotpath-alloc -p kithara-decode -- --test-threads=1

# Конкретный тест
cargo test --features hotpath profile_async_worker_throughput -- --test-threads=1

# Integration тесты
cargo test --features hotpath --test performance_profiling -- --test-threads=1
```

## Следующий шаг

**Рекомендуемое начало:**
1. Implement Phase 1 (Foundation) из HOTPATH_INTEGRATION_PLAN.md
2. Начать с 3-5 самых критичных тестов:
   - `profile_async_worker_throughput` (kithara-worker)
   - `profile_mp3_decode_chunks` (kithara-decode)
   - `profile_prefetch_worker_latency` (kithara-stream)
   - `profile_full_mp3_decode` (integration)
   - `profile_decode_pipeline_memory` (integration с alloc)

3. Собрать baseline метрики
4. Итеративно расширять coverage
