# TEST INVENTORY

Полный inventory всех тестов в проекте Kithara с классификацией и планом реорганизации.

**Дата создания**: 2026-01-20
**Общая статистика**:
- Integration-тесты в tests/: 41 файл
- Файлы с unit-тестами в src/: 9 файлов
- Unit-тесты в tests/ (требуют переноса): ~6 файлов (~1,014 строк)
- Integration-тесты в src/ (требуют переноса): 3 файла
- Монолитные файлы (>500 строк): 4 файла (~2,979 строк)

---

## Крейт: kithara-assets

**Статистика**:
- Tests в tests/: 9 файлов
- Tests в src/: 0 файлов

### Unit-тесты в tests/ (требуют переноса в src/)

- [ ] `tests/asset_id.rs` (110 строк) → `src/lib.rs`
  - Тестирует AssetId::from_url() изолированно
  - Критерии unit-теста: чистая логика без I/O, уже использует rstest
  - ✓ Правильно классифицирован

- [ ] `tests/canonicalization.rs` (63 строки) → `src/lib.rs`
  - Тестирует canonicalize_for_asset() изолированно
  - Критерии unit-теста: чистая логика без I/O, уже использует rstest
  - ✓ Правильно классифицирован

### Integration-тесты (правильно в tests/)

- [x] `tests/resource_path_test.rs` (192 строки)
  - Тестирует path() метод AssetResource с реальной FS
  - ✓ Правильно: integration тест с tempdir и I/O
  - **Примечание**: изначально классифицирован как unit-тест, но это integration

- [x] `tests/integration_storage_assets.rs` (594 строки)
  - Тестирует интеграцию DiskAssetStore с файловой системой
  - ✓ Правильно: tempdir, реальный I/O

- [x] `tests/streaming_resources_comprehensive.rs` (277 строк)
  - Тестирует StreamingResource с параллельными операциями
  - ✓ Правильно: сложные сценарии, async I/O

- [x] `tests/eviction_integration.rs`
  - Тестирует EvictAssets с реальным хранилищем
  - ✓ Правильно: интеграция компонентов

- [x] `tests/eviction_bytes_integration.rs`
  - Тестирует eviction по размеру байтов
  - ✓ Правильно: интеграция с FS

- [x] `tests/pins_index_integration.rs`
  - Тестирует pins index с персистентностью
  - ✓ Правильно: интеграция с storage

- [x] `tests/processing_integration.rs`
  - Тестирует process_and_cache с реальным хранилищем
  - ✓ Правильно: интеграция компонентов

### Проблемы

- **Unit в tests/**: 2 файла (173 строки) требуют переноса в src/
- **Покрытие**: нет unit-тестов для базовых функций в src/
- **Дублирование**: нет
- **Монолиты**: нет

---

## Крейт: kithara-bufpool

**Статистика**:
- Tests в tests/: 0 файлов
- Tests в src/: 1 файл

### Unit-тесты в src/ (правильно)

- [x] `src/lib.rs` - содержит unit-тесты для BufferPool
  - ✓ Правильно: простые тесты изолированной структуры

### Проблемы

- **Покрытие**: хорошее, можно добавить тесты для:
  - Concurrent access
  - PooledSlice drop behavior

---

## Крейт: kithara-decode

**Статистика**:
- Tests в tests/: 10 файлов
- Tests в src/: 1 файл

### Unit-тесты в tests/ (ПЕРЕСМОТРЕНО: это integration-тесты)

- [x] `tests/pipeline_unit_test.rs` (227 строк) - ОСТАЕТСЯ в tests/
  - **Пересмотрено**: это integration-тесты, не unit-тесты
  - Использует async/tokio, sleep(), каналы, сложный Pipeline
  - Mock decoder не делает их unit-тестами
  - Критерии integration-теста: ✓ async runtime, ✓ не детерминистично, ✓ сложная система
  - ✓ Правильно: остается в tests/

### Integration-тесты (правильно в tests/)

- [x] `tests/fixture_integration.rs`
  - Фикстура для интеграционных тестов

- [x] `tests/fixture.rs`
  - Общая фикстура (mock decoder)

- [x] `tests/decoder_tests.rs`
  - Тестирует реальный Symphonia decoder
  - ✓ Правильно: интеграция с внешней библиотекой

- [x] `tests/source_reader_tests.rs`
  - Тестирует SourceReader с реальным Source
  - ✓ Правильно: интеграция компонентов

- [x] `tests/decode_source_test.rs`
  - Тестирует DecodeSource с реальным декодером
  - ✓ Правильно: полная интеграция

- [x] `tests/mock_decoder_cursor_test.rs`
  - Mock decoder для тестов
  - ✓ Правильно: утилита для тестов

- [x] `tests/mock_decoder.rs`
  - Mock decoder implementation
  - ✓ Правильно: тестовая утилита

- [x] `tests/decode_hls_abr_test.rs`
  - Тестирует декодирование HLS с ABR switching
  - ✓ Правильно: сложная интеграция

- [x] `tests/hls_abr_variant_switch.rs`
  - Тестирует переключение вариантов во время декодирования
  - ✓ Правильно: интеграция HLS + decode

### Unit-тесты в src/ (правильно)

- [x] `src/pipeline.rs` - содержит несколько unit-тестов
  - ✓ Правильно: изолированные тесты логики

### Проблемы

- **Нет проблем**: все тесты правильно классифицированы

---

## Крейт: kithara-file

**Статистика**:
- Tests в tests/: 1 файл
- Tests в src/: 0 файлов

### Integration-тесты (правильно в tests/)

- [x] `tests/file_source.rs`
  - Тестирует FileSource с реальными HTTP запросами и файлами
  - ✓ Правильно: интеграция net + storage

### Проблемы

- Нет

---

## Крейт: kithara-hls

**Статистика**:
- Tests в tests/: 12 файлов
- Tests в src/: 4 файла

### Unit-тесты в tests/ (частично перенесены)

- [x] `tests/unit_tests.rs` (315 строк) → `src/parsing.rs`
  - ✓ Перенесено: 13 тестов (23 с rstest параметризацией)
  - Тестирует parse_master_playlist, parse_media_playlist, VariantId
  - Все тесты проходят

- [x] `tests/driver_test.rs` (107 строк) - ОСТАЕТСЯ в tests/
  - **Пересмотрено**: это integration-тесты, не unit-тесты
  - Использует async/tokio, AbrTestServer (HTTP), sleep(), реальный I/O
  - Тестирует полную интеграцию Hls::open с чтением данных
  - ✓ Правильно: остается в tests/

### Integration-тесты (правильно в tests/)

- [x] `tests/basic_playback.rs`
  - Тестирует базовый playback сценарий
  - ✓ Правильно: полная интеграция компонентов

- [x] `tests/smoke_test.rs`
  - Smoke тесты для HLS
  - ✓ Правильно: интеграция

- [x] `tests/abr_integration.rs`
  - Тестирует ABR в реальных условиях
  - ✓ Правильно: интеграция с HTTP server

- [x] `tests/basestream_iter.rs` (700 строк) **МОНОЛИТ**
  - Тестирует итерацию по BaseStream
  - ✓ Правильно: integration тест
  - ⚠ Проблема: монолитный файл (требует разбиения)

- [x] `tests/keys_integration.rs`
  - Тестирует KeyManager с реальным хранилищем
  - ✓ Правильно: интеграция

- [x] `tests/playlist_integration.rs`
  - Тестирует PlaylistManager
  - ✓ Правильно: интеграция

- [x] `tests/fixture.rs` (639 строк) **МОНОЛИТ**
  - Общие фикстуры для HLS тестов
  - ✓ Правильно: фикстуры для integration тестов
  - ⚠ Проблема: монолитный файл (требует разбиения)

- [x] `tests/sync_reader_hls_test.rs`
  - Тестирует sync reader для HLS
  - ✓ Правильно: интеграция

- [x] `tests/deferred_abr.rs`
  - Тестирует отложенный ABR
  - ✓ Правильно: интеграция

- [x] `tests/source_seek.rs`
  - Тестирует seek операции
  - ✓ Правильно: интеграция

### Unit-тесты/Integration в src/ (требуют анализа)

- [ ] `src/events.rs` (строки 49-85) - содержит integration тест
  - **Integration тест**: test_broadcast_channel()
  - ⚠ Требует переноса в tests/

- [x] `src/abr/estimator.rs` - содержит 1 unit-тест
  - ✓ Правильно: unit-тест в src/
  - ⚠ Недостаточное покрытие (только 1 тест)

- [x] `src/abr/controller.rs` - возможно содержит тесты
  - Нужно проверить

- [x] `src/index.rs` - возможно содержит тесты
  - Нужно проверить

### Проблемы

- **Unit в tests/**: ~~2 файла~~ 1 файл (315 строк) ✓ перенесен в src/parsing.rs
- **Integration в src/**: 1 тест требует переноса в tests/
- **Монолиты**: 2 файла (1,339 строк) требуют разбиения:
  - `basestream_iter.rs` (700 строк)
  - `fixture.rs` (639 строк)
- **Недостаточное покрытие**: `abr/estimator.rs` - только 1 тест

---

## Крейт: kithara-net

**Статистика**:
- Tests в tests/: 5 файлов
- Tests в src/: 0 файлов

### Unit-тесты в tests/ (требуют переноса в src/)

- [ ] `tests/types.rs` (371 строка) → `src/types.rs`
  - Тестирует Headers, RangeSpec, RetryPolicy изолированно
  - Критерии unit-теста: чистая логика без I/O, уже использует rstest
  - ✓ Хорошая параметризация rstest
  - **Примечание**: очень хорошо написанные тесты, готовы к переносу

- [ ] `tests/error.rs` (295 строк) → `src/error.rs`
  - Тестирует NetError типы и методы изолированно
  - Критерии unit-теста: чистая логика без I/O, уже использует rstest
  - ✓ Хорошая параметризация rstest
  - **Примечание**: готовы к переносу

### Integration-тесты (правильно в tests/)

- [x] `tests/http_client.rs` (1,119 строк) **МОНОЛИТ**
  - Тестирует HttpClient с реальным HTTP server
  - ✓ Правильно: integration тест
  - ⚠ Проблема: монолитный файл (требует разбиения на несколько модулей)

- [x] `tests/retry.rs`
  - Тестирует retry logic
  - ✓ Правильно: integration тест с HTTP

- [x] `tests/timeout.rs`
  - Тестирует timeout behavior
  - ✓ Правильно: integration тест

### Проблемы

- **Unit в tests/**: 2 файла (666 строк) требуют переноса в src/
- **Монолит**: `http_client.rs` (1,119 строк) требует разбиения на:
  - `http_client_basic.rs` (~300 строк)
  - `http_client_range.rs` (~250 строк)
  - `http_client_headers.rs` (~200 строк)
  - `http_client_retry.rs` (~250 строк)
  - `fixture.rs` (~119 строк)

---

## Крейт: kithara-storage

**Статистика**:
- Tests в tests/: 2 файла
- Tests в src/: 0 файлов

### Integration-тесты (правильно в tests/)

- [x] `tests/atomic.rs`
  - Тестирует AtomicResource с файловой системой
  - ✓ Правильно: integration тест с I/O

- [x] `tests/streaming.rs`
  - Тестирует StreamingResource
  - ✓ Правильно: integration тест с I/O

### Проблемы

- Нет

---

## Крейт: kithara-stream

**Статистика**:
- Tests в tests/: 2 файла
- Tests в src/: 2 файла

### Integration-тесты (правильно в tests/)

- [x] `tests/source.rs` (521 строк) **МОНОЛИТ**
  - Тестирует Source с различными сценариями
  - ✓ Правильно: integration тест
  - ⚠ Проблема: монолитный файл (требует разбиения)

- [x] `tests/sync_reader_basic_test.rs`
  - Тестирует SyncReader
  - ✓ Правильно: integration тест

### Unit-тесты в src/ (требуют анализа)

- [ ] `src/prefetch.rs` (строки 40-120) - содержит ДУБЛИРУЮЩИЕ тесты
  - **Дублирует**: kithara-worker тесты
  - `test_prefetch_worker_backward_compat()` - дублирует worker test
  - `test_prefetch_consumer_backward_compat()` - тривиальный тест
  - ⚠ **Требует удаления** (Фаза 4)

- [x] `src/media_info.rs` - содержит unit-тесты
  - ✓ Правильно: unit-тесты codec parsing
  - ⚠ Можно добавить rstest параметризацию

### Проблемы

- **Монолит**: `source.rs` (521 строк) требует разбиения на:
  - `source_basic.rs` (~200 строк)
  - `source_seek.rs` (~180 строк)
  - `source_edge_cases.rs` (~141 строк)
- **Дублирование**: `src/prefetch.rs` дублирует тесты из kithara-worker (2 теста, ~80 строк)

---

## Крейт: kithara-worker

**Статистика**:
- Tests в tests/: 0 файлов
- Tests в src/: 1 файл

### Integration-тесты в src/ (требуют переноса в tests/)

- [ ] `src/lib.rs` (строки 480-706) → `tests/worker_integration.rs`
  - **Integration тесты**: test_async_worker_basic(), test_sync_worker_basic()
  - Включает TestAsyncSource и TestSyncSource
  - ⚠ Требует переноса в tests/ корня проекта

### Проблемы

- **Integration в src/**: 1 файл требует переноса в tests/

---

## Сводка проблем

### 1. Unit-тесты в tests/ (требуют переноса в src/)

| Крейт | Файл | Строк | Целевой файл в src/ |
|-------|------|-------|---------------------|
| Крейт | Файл | Строк | Целевой файл в src/ | Статус |
|-------|------|-------|---------------------|--------|
| kithara-assets | asset_id.rs | 110 | src/key.rs | ✓ Перенесено |
| kithara-assets | canonicalization.rs | 63 | src/key.rs | ✓ Перенесено |
| kithara-hls | unit_tests.rs | 315 | src/parsing.rs | ✓ Перенесено |
| kithara-net | types.rs | 371 | src/types.rs | Осталось |
| kithara-net | error.rs | 295 | src/error.rs | Осталось |
| **Итого** | **5 файлов** | **1,154** | **3 ✓ / 2 осталось** |

### 2. Integration-тесты в src/ (требуют переноса в tests/)

| Крейт | Файл | Строки | Целевой файл в tests/ |
|-------|------|--------|----------------------|
| kithara-worker | src/lib.rs | 480-706 | tests/worker_integration.rs |
| kithara-hls | src/events.rs | 49-85 | tests/events_integration.rs |
| **Итого** | **2 файла** | **~263** | |

### 3. Монолитные файлы (>500 строк, требуют разбиения)

| Крейт | Файл | Строк | План разбиения |
|-------|------|-------|---------------|
| kithara-net | http_client.rs | 1,119 | 5 файлов: basic, range, headers, retry, fixture |
| kithara-hls | fixture.rs | 639 | 4 файла: server, assets, crypto, abr |
| kithara-hls | basestream_iter.rs | 700 | 3 файла: basic, seek, abr |
| kithara-stream | source.rs | 521 | 3 файла: basic, seek, edge_cases |
| **Итого** | **4 файла** | **2,979** | **15 новых файлов** |

### 4. Дубликаты (требуют удаления)

| Файл | Дублирует | Тесты | Решение |
|------|-----------|-------|---------|
| kithara-stream/src/prefetch.rs | kithara-worker тесты | test_prefetch_worker_backward_compat, test_prefetch_consumer_backward_compat | Удалить оба теста |
| kithara-decode/tests/pipeline_unit_test.rs | src/pipeline.rs (?) | Нужно проверить | Проверить и удалить дубликаты |

### 5. Недостаточное покрытие

- `kithara-hls/src/abr/estimator.rs` - только 1 тест (нужно добавить)
- `kithara-bufpool/src/lib.rs` - можно добавить тесты для concurrent access
- `kithara-stream/src/media_info.rs` - можно параметризовать с rstest

### 6. rstest параметризация

**Файлы без rstest** (~24% integration тестов):
- `kithara-decode/tests/mock_decoder.rs`
- `kithara-storage/tests/streaming.rs`
- `kithara-file/tests/file_source.rs`
- Многие unit-тесты в src/

---

## План переноса по фазам

### Фаза 2: Перенос unit-тестов из tests/ в src/ (8 файлов, 1,680 строк)

```
✓ kithara-assets/tests/resource_path_test.rs (192) → src/lib.rs
✓ kithara-assets/tests/asset_id.rs (110) → src/lib.rs
✓ kithara-assets/tests/canonicalization.rs (63) → src/lib.rs
✓ kithara-decode/tests/pipeline_unit_test.rs (227) → src/pipeline.rs
✓ kithara-hls/tests/unit_tests.rs (315) → src/abr/controller.rs + src/index.rs
✓ kithara-hls/tests/driver_test.rs (107) → src/lib.rs
✓ kithara-net/tests/types.rs (371) → src/types.rs
✓ kithara-net/tests/error.rs (295) → src/error.rs
```

### Фаза 3: Перенос integration-тестов из src/ в tests/

```
✓ kithara-worker/src/lib.rs → tests/worker_integration.rs
✓ kithara-hls/src/events.rs → tests/events_integration.rs
✓ Переместить все crates/*/tests/ → tests/*/
```

### Фаза 4: Оптимизация unit-тестов

```
✓ Удалить дубликаты (prefetch, pipeline)
✓ Добавить rstest параметризацию
✓ Добавить недостающие тесты (estimator, bufpool)
✓ Сделать структуры generic для mock-изоляции
```

### Фаза 5: Оптимизация integration-тестов

```
✓ Создать tests/common/ с общими фикстурами
✓ Разбить монолиты:
  - http_client.rs → 5 файлов
  - fixture.rs → 4 файла
  - basestream_iter.rs → 3 файла
  - source.rs → 3 файла
✓ Добавить rstest параметризацию
✓ Оптимизировать фикстуры
```

---

## Верификация

После создания этого inventory выполнить:

```bash
# Подтвердить структуру
cargo test --workspace --lib    # unit-тесты
cargo test --workspace --test '*'  # integration-тесты

# Подсчет тестов
grep -r "#\[test\]" crates/*/src | wc -l
grep -r "#\[tokio::test\]" crates/*/tests | wc -l
grep -r "#\[rstest\]" crates/*/tests | wc -l
```

**Следующий шаг**: Начать Фазу 2 - перенос unit-тестов из tests/ в src/
