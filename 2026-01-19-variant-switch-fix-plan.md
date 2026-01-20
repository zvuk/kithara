# План исправления ABR Variant Switch через TDD

## Корневая причина бага

### Последовательность событий:
1. ABR переключается с variant 0 на variant 2
2. Pipeline **заранее** загружает segments 4, 5, 6 из variant 2
3. `VariantIndex::add()` устанавливает `first_media_segment = 4` (первый загруженный)
4. Decoder пытается прочитать offset 203600, не находит в variant 2
5. `find_segment_index_for_offset(203600)` находит segment #3 в variant 0
6. Pipeline делает `seek(3)` → загружает segment #3 из variant 2
7. Segment #3 добавляется в BTreeMap: `[3, 4, 5, 6]`
8. **ПРОБЛЕМА**: `first_media_segment` всё еще = 4 (НЕ обновляется!)
9. При следующем `find(offset)`:
   - expected_media_idx = 4
   - Но сегменты начинаются с #3
   - Кумулятивные offsets считаются от #3: 0..len3, len3..len3+len4, ...
   - Offset для segment #4 сдвигается!

### Код с багом:

```rust
// index.rs:105-107
if self.first_media_segment.is_none() {
    self.first_media_segment = Some(segment_index);  // ← Устанавливается ОДИН РАЗ!
}
```

### Почему это критично:
- HLS ABR может начать загрузку с любого сегмента (например, #4)
- Seek может добавить более ранний сегмент (#3) позже
- `first_media_segment` должен отражать МИНИМАЛЬНЫЙ segment_index в BTreeMap
- Gap detection полагается на правильный `first_media_segment`

---

## Обнаруженные проблемы (2026-01-20)

### Проблема 1: VariantIndex::find() неправильно вычисляет cumulative offset после ABR switch

**Воспроизведение**:
1. Загружены segments: variant=0 seg=0,1; variant=2 seg=2
2. ABR switch на variant 2
3. Decoder читает sequential от offset 400000 (начало seg=2)
4. `find(400000, variant=0)` возвращает None (сегмент не загружен)
5. `find(400000, variant=2)` возвращает None (cumulative offset неправильный!)

**Корневая причина**:
- `VariantIndex::find()` вычисляет cumulative offset от 0
- Предполагает что загружены ВСЕ сегменты начиная с `first_media_segment`
- При ABR: variant=2 имеет только seg=2, first_media_segment=2
- Cumulative offset: 0..200000 (НЕПРАВИЛЬНО! должно быть 400000..600000)

**Решение**:
- Вариант A: SegmentStream отправляет `global_offset` для каждого сегмента (большое изменение)
- Вариант B: VariantIndex вычисляет offset от `first_media_segment * avg_segment_size`
- Вариант C: VOD HLS хранит абсолютные timestamp в playlist и пересчитывает offsets

### Проблема 2: HLS Stream завершается до обработки seek после ABR switch

**Воспроизведение**:
1. Pipeline загружает все сегменты из playlist (seg 0,1 variant=0, seg 2 variant=2)
2. `SegmentStream` завершается, отправляет `None`
3. `HlsAdapter` устанавливает `index.finished = true`
4. Decoder читает offset 400000, вызывает `wait_range()`
5. `wait_range()` отправляет `seek(2)` для variant=0
6. **Seek игнорируется** - pipeline уже остановлен!

**Решение**:
- HLS driver не должен завершаться пока есть активные readers
- Или: wait_range() должен reject EOF если есть pending seek

---

## TDD План исправления

### Фаза -1: Изолированные тесты индексов (КРИТИЧНО!)

**Проблема**: Тесты 1-9 проверяют всю цепочку, но не изолируют VariantIndex/SegmentIndex логику.

**Требования**:
- ✅ Минимум 10 сегментов в тестах
- ✅ Минимум 3 варианта (лучше 5 для покрытия edge cases)
- ✅ Все тесты параметризованы через `rstest`
- ✅ Проверка gap detection, offset calculation, first_media_segment tracking

**Тесты**:

#### Тест Index-1: VariantIndex isolated - sequential add (PARAMETRIZED)
```rust
#[rstest]
#[case(10, 200_000)]  // 10 segments, 200KB each
#[case(15, 150_000)]  // 15 segments, 150KB each
#[case(20, 100_000)]  // 20 segments, 100KB each
fn test_variant_index_sequential(
    #[case] num_segments: usize,
    #[case] segment_size: u64,
) {
    let mut idx = VariantIndex::new();

    // Добавляем segments 0..num_segments последовательно
    for seg in 0..num_segments {
        idx.add(url(seg), segment_size, seg, None);
    }

    // Проверяем find() для каждого offset
    for seg in 0..num_segments {
        let offset = seg as u64 * segment_size;
        let entry = idx.find(offset).expect("segment not found");
        assert_eq!(entry.segment_index, seg);
        assert_eq!(entry.global_start, offset);
        assert_eq!(entry.global_end, offset + segment_size);
    }

    // Проверяем first_media_segment
    assert_eq!(idx.first_media_segment, Some(0));
}
```

#### Тест Index-2: VariantIndex isolated - ABR switch (segments out of order, PARAMETRIZED)
```rust
#[rstest]
#[case(4, 10, 200_000)]  // Start at seg 4, total 10 segments
#[case(5, 12, 150_000)]  // Start at seg 5, total 12 segments
#[case(7, 15, 100_000)]  // Start at seg 7, total 15 segments
fn test_variant_index_abr_out_of_order(
    #[case] start_seg: usize,
    #[case] total_segments: usize,
    #[case] segment_size: u64,
) {
    let mut idx = VariantIndex::new();

    // Сценарий ABR:
    // 1. Add segments start_seg..total_segments (ABR начал с mid-stream)
    for seg in start_seg..total_segments {
        idx.add(url(seg), segment_size, seg, None);
    }

    assert_eq!(idx.first_media_segment, Some(start_seg));

    // 2. Seek backward: Add segments 0..start_seg
    for seg in 0..start_seg {
        idx.add(url(seg), segment_size, seg, None);
    }

    // CRITICAL: first_media_segment должен обновиться на 0!
    assert_eq!(idx.first_media_segment, Some(0),
        "first_media_segment should update to 0 after seek backward");

    // Проверяем cumulative offsets для ВСЕХ сегментов
    for seg in 0..total_segments {
        let offset = seg as u64 * segment_size;
        let entry = idx.find(offset).expect(&format!("segment {} not found", seg));
        assert_eq!(entry.segment_index, seg);
        assert_eq!(entry.global_start, offset,
            "segment {} has wrong global_start", seg);
        assert_eq!(entry.global_end, offset + segment_size,
            "segment {} has wrong global_end", seg);
    }
}
```

#### Тест Index-3: SegmentIndex - multi-variant (PARAMETRIZED)
```rust
#[rstest]
#[case(3, 10, 200_000)]   // 3 variants, 10 segments each
#[case(5, 12, 150_000)]   // 5 variants, 12 segments each
#[case(7, 15, 100_000)]   // 7 variants, 15 segments each
fn test_segment_index_multi_variant(
    #[case] num_variants: usize,
    #[case] segments_per_variant: usize,
    #[case] segment_size: u64,
) {
    let mut idx = SegmentIndex::new();

    // Добавляем сегменты для всех вариантов
    for variant in 0..num_variants {
        for seg in 0..segments_per_variant {
            idx.add(url(variant, seg), segment_size, variant, seg, None);
        }
    }

    // Проверяем find(offset, variant) для каждого варианта
    for variant in 0..num_variants {
        for seg in 0..segments_per_variant {
            let offset = seg as u64 * segment_size;
            let entry = idx.find(offset, variant)
                .expect(&format!("variant {} segment {} not found", variant, seg));

            assert_eq!(entry.segment_index, seg);
            assert_eq!(entry.global_start, offset);
            assert_eq!(entry.global_end, offset + segment_size);
        }
    }

    // find_segment_index_for_offset() должен найти в ЛЮБОМ варианте
    for seg in 0..segments_per_variant {
        let offset = seg as u64 * segment_size;
        assert_eq!(idx.find_segment_index_for_offset(offset), Some(seg));
    }
}
```

#### Тест Index-4: ABR switch scenario - incomplete variants (PARAMETRIZED)
```rust
#[rstest]
#[case(3, 10, 2, 200_000)]  // 3 variants, 10 total segs, switch at seg 2
#[case(5, 15, 5, 150_000)]  // 5 variants, 15 total segs, switch at seg 5
#[case(7, 20, 8, 100_000)]  // 7 variants, 20 total segs, switch at seg 8
fn test_segment_index_abr_switch_incomplete(
    #[case] num_variants: usize,
    #[case] total_segments: usize,
    #[case] switch_at_seg: usize,
    #[case] segment_size: u64,
) {
    let mut idx = SegmentIndex::new();
    let old_variant = 0;
    let new_variant = num_variants - 1;  // Switch to last variant

    // Полный сценарий ABR:
    // 1. variant=old_variant: add segments 0..switch_at_seg
    for seg in 0..switch_at_seg {
        idx.add(url(old_variant, seg), segment_size, old_variant, seg, None);
    }

    // 2. ABR switch на new_variant
    // 3. variant=new_variant: add segments switch_at_seg..total_segments
    for seg in switch_at_seg..total_segments {
        idx.add(url(new_variant, seg), segment_size, new_variant, seg, None);
    }

    // Проверяем что индекс правильно обрабатывает:

    // - Segments 0..switch_at_seg доступны только в old_variant
    for seg in 0..switch_at_seg {
        let offset = seg as u64 * segment_size;

        assert!(idx.find(offset, old_variant).is_some(),
            "segment {} should exist in old variant {}", seg, old_variant);

        assert!(idx.find(offset, new_variant).is_none(),
            "segment {} should NOT exist in new variant {}", seg, new_variant);
    }

    // - Segments switch_at_seg..total доступны только в new_variant
    for seg in switch_at_seg..total_segments {
        let offset = seg as u64 * segment_size;

        assert!(idx.find(offset, new_variant).is_some(),
            "segment {} should exist in new variant {}", seg, new_variant);

        // CRITICAL: Проверяем что global_start ПРАВИЛЬНЫЙ!
        // Это тест для бага cumulative offset!
        let entry = idx.find(offset, new_variant).unwrap();
        assert_eq!(entry.global_start, offset,
            "segment {} in new variant has WRONG global_start (expected {}, got {})",
            seg, offset, entry.global_start);
    }

    // - find_segment_index_for_offset() должен найти в ЛЮБОМ варианте
    for seg in 0..total_segments {
        let offset = seg as u64 * segment_size;
        assert_eq!(idx.find_segment_index_for_offset(offset), Some(seg),
            "find_segment_index_for_offset({}) failed", offset);
    }
}
```

#### Тест Index-5: Gap detection (PARAMETRIZED)
```rust
#[rstest]
#[case(10, 5, 200_000)]  // 10 total, gap at seg 5
#[case(15, 8, 150_000)]  // 15 total, gap at seg 8
#[case(20, 12, 100_000)] // 20 total, gap at seg 12
fn test_variant_index_gap_detection(
    #[case] total_segments: usize,
    #[case] gap_at: usize,
    #[case] segment_size: u64,
) {
    let mut idx = VariantIndex::new();

    // Add all segments EXCEPT gap_at
    for seg in 0..total_segments {
        if seg == gap_at {
            continue;  // Skip - create gap
        }
        idx.add(url(seg), segment_size, seg, None);
    }

    // find() should return None for offsets >= gap_at (gap detected)
    for seg in 0..gap_at {
        let offset = seg as u64 * segment_size;
        assert!(idx.find(offset).is_some(),
            "segment {} before gap should be found", seg);
    }

    for seg in gap_at..total_segments {
        let offset = seg as u64 * segment_size;
        assert!(idx.find(offset).is_none(),
            "segment {} after gap should NOT be found (gap detection)", seg);
    }
}
```

**Файлы для тестов**:
- ✅ `crates/kithara-hls/src/index.rs` (#[cfg(test)] mod tests) - ГОТОВО, 15/15 PASS
- Добавить `rstest = "0.23"` в `dev-dependencies` в `Cargo.toml` - ГОТОВО

**Статус**: ✅ ВСЕ ТЕСТЫ ИНДЕКСОВ ПРОШЛИ (15/15)
- Фикс cumulative offset в VariantIndex::find() применён

---

### Фаза -2: Изолированные тесты HLS Driver (КРИТИЧНО!)

**Проблема**: Тест 9 показал что HLS driver завершается до обработки seek после ABR switch.

**Корневая причина**:
```
1. Pipeline загружает segments: variant=0 seg 0,1; variant=2 seg 2
2. SegmentStream.next() -> None (все сегменты из playlist загружены)
3. HlsAdapter устанавливает index.finished = true
4. wait_range() получает offset 400000
5. wait_range() отправляет seek(2) для variant 0
6. ❌ Seek НЕ ОБРАБАТЫВАЕТСЯ - pipeline уже остановлен!
```

**Архитектурная проблема**:
- HLS driver не знает сколько consumer'ов активно
- Driver завершается когда playlist закончился, НО decoder может быть в середине чтения
- Seek после "finished" игнорируется

**Решение**: Тесты должны проверить:
1. Driver обрабатывает seek ПОСЛЕ finished
2. Driver не завершается пока есть pending operations
3. Driver корректно обрабатывает ABR switch + seek backward

**Компонентная схема HLS:**

```
User Request (offset 400000)
    ↓
HlsSource::wait_range()  ← Source trait implementation
    ↓
SegmentIndex::find(offset, variant)  ← Index lookup
    ↓
PipelineHandle::seek(segment_index)  ← Command to driver
    ↓
HlsDriver (SegmentStream)  ← Loads segments
    ↓
FetchManager  ← HTTP downloads
```

**Тесты**:

#### Тест Driver-1: Pipeline seeks after playlist finished (ISOLATED)

**Цель**: Проверить что pipeline обрабатывает seek ПОСЛЕ завершения playlist.

```rust
#[tokio::test]
async fn test_pipeline_seek_after_finished() {
    // 1. Create pipeline with 3 segments
    // 2. Read all segments (SegmentStream returns None)
    // 3. Pipeline should be "finished" but still alive
    // 4. Send seek(1) command
    // 5. Verify segment 1 loaded AGAIN

    // EXPECTED: seek processed
    // WITHOUT FIX: seek ignored (pipeline stopped)
}
```

#### Тест Driver-2: ABR switch + seek backward (ISOLATED)

**Цель**: Проверить ABR switch с seek backward для пропущенных сегментов.

```rust
#[rstest]
#[case(0, 2, 2)]  // variant 0 -> 2, seek back to seg 2
#[case(0, 3, 5)]  // variant 0 -> 3, seek back to seg 5
async fn test_abr_switch_seek_backward(
    #[case] old_variant: usize,
    #[case] new_variant: usize,
    #[case] seek_segment: usize,
) {
    // 1. Load segments 0..5 from variant 0
    // 2. ABR switch to new_variant
    // 3. Load segments 5..10 from new_variant
    // 4. Decoder seeks back to segment 2 (needs variant 0!)
    // 5. Verify segment 2 loaded from variant 0

    // EXPECTED: segment loaded from OLD variant
    // WITHOUT FIX: EOF or wrong data from new variant
}
```

#### Тест Driver-3: FetchManager isolation (MOCK HTTP)

**Цель**: Тестировать FetchManager без реального HTTP сервера.

```rust
// Generic FetchManager over trait FetchBackend
trait FetchBackend {
    async fn fetch(&self, url: Url) -> Result<Bytes>;
}

struct MockFetchBackend {
    responses: HashMap<Url, Bytes>,
}

#[rstest]
#[case(3, 10)]  // 3 variants, 10 segments each
async fn test_fetch_manager_mock(
    #[case] variants: usize,
    #[case] segments: usize,
) {
    let backend = MockFetchBackend::with_test_data(variants, segments);
    let fetcher = FetchManager::new(backend);

    // Test concurrent fetches, retries, caching
}
```

#### Тест Driver-4: Full HLS driver chain (INTEGRATION)

**Цель**: Полная цепочка HLS без decoder.

```rust
#[rstest]
#[case(3, 10, 5, 2)]  // 3 vars, 10 segs, switch at 5, seek back to 2
async fn test_hls_driver_full_chain(
    #[case] num_variants: usize,
    #[case] total_segments: usize,
    #[case] switch_at: usize,
    #[case] seek_back_to: usize,
) {
    // 1. Start HLS with variant 0
    // 2. Load segments 0..switch_at
    // 3. ABR switch to variant 1
    // 4. Load segments switch_at..total
    // 5. Sequential reader at offset for seek_back_to
    // 6. Verify driver loads segment from variant 0

    // Tests: Index + Adapter + Pipeline + FetchManager
}
```

**Матрица тестов - Все компоненты и комбинации**:

```
Компоненты:
├── VariantIndex (5 tests) ✅ PASS 15/15
├── SegmentIndex (5 tests) ✅ PASS 15/15
├── HlsAdapter::wait_range() (pending)
├── PipelineHandle (pending)
├── SegmentStream/Driver (pending)
└── FetchManager (pending)

Комбинации:
├── Index + Adapter (pending)
├── Adapter + Pipeline (pending)
├── Pipeline + FetchManager (pending)
├── Full HLS chain without decoder ✅ PASS (Тест 8)
└── Full HLS + Decoder + ABR ❌ FAIL (Тест 9) - driver завершается рано
```

**Файлы для изменений**:
- `crates/kithara-hls/src/stream/pipeline.rs` - возможно generic over Backend
- `crates/kithara-hls/tests/driver_test.rs` (новый)
- `crates/kithara-hls/tests/fetch_manager_test.rs` (новый, если делаем generic)

---

### Фаза 0: Сделать decoder generic для поддержки mock decoder (ПОДГОТОВКА)

**КРИТИЧНО**: Перед написанием интеграционного теста нужно сделать decode pipeline generic!

**Проблема**:
- Сейчас `Pipeline` жестко привязан к Symphonia decoder (AAC/MP3/FLAC)
- Для тестов нужен mock decoder который работает с текстовыми сегментами "V0-SEG-0:AAA..."
- Без generic decoder невозможно протестировать полную связку decode+HLS+ABR

**Решение**:
1. Добавить trait `Decoder` в kithara-decode:
   ```rust
   pub trait Decoder: Send {
       type Sample;

       fn decode_packet(&mut self, data: &[u8]) -> Result<Vec<Self::Sample>>;
       fn sample_rate(&self) -> u32;
       fn channels(&self) -> usize;
   }
   ```

2. Сделать `Pipeline<D: Decoder>` generic:
   ```rust
   pub struct Pipeline<D: Decoder> {
       decoder: D,
       // ...
   }
   ```

3. Создать `SymphoniaDecoder` для production:
   ```rust
   pub struct SymphoniaDecoder {
       // existing Symphonia logic
   }

   impl Decoder for SymphoniaDecoder {
       type Sample = f32;
       // ...
   }
   ```

4. Создать `MockDecoder` для тестов:
   ```rust
   #[cfg(test)]
   pub struct MockDecoder {
       // Парсит "V{variant}-SEG-{segment}:" из байтов
       // Возвращает синтетические PCM samples с variant/segment метаданными
   }

   impl Decoder for MockDecoder {
       type Sample = f32;

       fn decode_packet(&mut self, data: &[u8]) -> Result<Vec<f32>> {
           // Парсим "V0-SEG-1:" -> генерируем samples с pattern
           // Sample pattern: [variant as f32, segment as f32, 0.0, 1.0, ...]
       }
   }
   ```

**Файлы для изменения**:
- `crates/kithara-decode/src/decoder.rs` (новый) - trait Decoder
- `crates/kithara-decode/src/pipeline.rs` - сделать generic
- `crates/kithara-decode/src/symphonia_decoder.rs` (новый) - переместить Symphonia logic
- `crates/kithara-decode/tests/mock_decoder.rs` (новый) - mock для тестов

**Ожидаемый результат**:
- Production код использует `Pipeline<SymphoniaDecoder>`
- Тесты используют `Pipeline<MockDecoder>`
- Все существующие тесты проходят

---

### Фаза 0.5: Пошаговое тестирование компонентов (ОБЯЗАТЕЛЬНО!)

**КРИТИЧНО**: Нельзя тестировать всю цепочку сразу! Нужно тестировать каждый компонент отдельно, потом добавлять по одному.

#### Цепочка компонентов:

```
MockDecoder (чтение binary формата)
    ↓
Pipeline + MockDecoder (decode + resampling)
    ↓
SyncReader (byte stream prefetch)
    ↓
StreamSource<Hls> (HLS orchestration)
    ↓
Полная цепочка: Pipeline + MockDecoder + SyncReader + StreamSource<Hls>
```

#### Тесты по компонентам (от простого к сложному):

- [x] **Тест 1**: MockDecoder изолированно (binary format parsing)
  - Файл: `mock_decoder.rs::tests`
  - Проверка: читает корректно binary сегменты, генерирует PCM
  - Статус: ✅ PASS (5 tests)

- [x] **Тест 2**: Pipeline + MockDecoder (без HLS)
  - Файл: `decode_source_test.rs::test_pipeline_reads_all_chunks_from_decoder`
  - Проверка: Pipeline читает ВСЕ chunks из MockDecoder
  - Статус: ✅ PASS (100 chunks читаются корректно)

- [x] **Тест 3**: Pipeline + SimpleMockDecoder (без resampling)
  - Файл: `pipeline_unit_test.rs`
  - Проверка: resampler опциональный, можно тестировать без него
  - Статус: ✅ PASS

- [ ] **Тест 4**: SyncReader + Cursor<Vec<u8>> (без HLS, статические данные)
  - Файл: `TODO: sync_reader_basic_test.rs`
  - Проверка: SyncReader читает все данные из статичного буфера
  - Данные: 3 binary сегмента в Cursor
  - Ожидается: читает все 3 сегмента

- [ ] **Тест 5**: MockDecoder + SyncReader + Cursor (без HLS)
  - Файл: `TODO: mock_decoder_sync_reader_test.rs`
  - Проверка: MockDecoder через SyncReader читает все сегменты
  - Данные: 10 binary сегментов в Cursor
  - Ожидается: все 10 сегментов декодируются

- [ ] **Тест 6**: Pipeline + MockDecoder + SyncReader + Cursor (без HLS)
  - Файл: `TODO: pipeline_sync_reader_test.rs`
  - Проверка: Полный pipeline с SyncReader
  - Данные: 10 binary сегментов в Cursor
  - Ожидается: все 10 chunks в output channel

- [ ] **Тест 7**: StreamSource<Hls> изолированно (HLS без decode)
  - Файл: `kithara-hls/tests/` (уже есть)
  - Проверка: HLS загружает все сегменты
  - Статус: ✅ PASS (протестировано в kithara-hls)

- [ ] **Тест 8**: SyncReader + StreamSource<Hls> (без MockDecoder)
  - Файл: `TODO: sync_reader_hls_test.rs`
  - Проверка: SyncReader читает все байты из HLS source
  - Данные: 3 сегмента по 200KB через AbrTestServer
  - Ожидается: читаются ВСЕ 3 сегмента (не 2!)

- [ ] **Тест 9**: Pipeline + MockDecoder + SyncReader + StreamSource<Hls>
  - Файл: `decode_hls_abr_test.rs`
  - Проверка: ПОЛНАЯ цепочка с ABR
  - Статус: ❌ FAIL (читает только 2 сегмента из 3)
  - **БЛОКЕР**: Тест 8 должен PASS перед этим!

#### Стратегия отладки:

1. ✅ Тест 4 PASS → Cursor читает все данные корректно
2. ✅ Тест 5 PASS → MockDecoder + Cursor читает все 10 сегментов
3. Тест 6 SKIP → слишком сложно (требует полную impl Source для Cursor)
4. **Тест 8 - КРИТИЧЕСКИЙ**:
   - Если PASS (все 3 сегмента) → проблема в MockDecoder или decode pipeline
   - Если FAIL (только 2 сегмента) → **проблема в HLS или SyncReader с HLS**
5. Если Тест 9 FAIL (а Тест 8 PASS) → проблема в MockDecoder или decode pipeline

#### План действий если Тест 8 FAIL (только 2 сегмента):

**Гипотезы**:
1. HLS не загружает 3-й сегмент (проблема в HlsDriver/FetchManager)
2. SyncReader неправильно определяет EOF для HLS source
3. HLS Source.len() возвращает неправильное значение после 2 сегментов

**Действия для отладки**:
1. Добавить логирование в HLS FetchManager - сколько сегментов загружено
2. Добавить логирование в SyncReader - когда и почему он считает EOF
3. Проверить HLS Source.len() - совпадает ли с реальным размером данных
4. Проверить кумулятивные offsets в VariantIndex - правильно ли считается размер
5. Создать минимальный тест HLS без SyncReader - просто Source.read_at() всех байтов

**Если проблема в HLS**:
- Значит ABR variant switch fix НЕ СВЯЗАН с этой проблемой
- Нужно сначала исправить HLS prefetch/loading, потом вернуться к ABR тесту

**Если проблема в SyncReader с HLS**:
- Проверить как SyncReader определяет EOF для streaming sources
- Возможно нужен другой механизм для источников где len() может меняться

**КРИТИЧНО**: Если Тест 8 FAIL, НЕ ПРОДОЛЖАТЬ с Тестом 9!
Сначала исправить причину, потом двигаться дальше.

**ТЕКУЩИЙ СТАТУС**: Тесты 1-5 PASS, Тест 6 SKIP, делаем Тест 8.

---

### Фаза 1: Написать failing test (RED)

**ВАЖНО**: Связка stream+hls УЖЕ ПРОТЕСТИРОВАНА в kithara-hls tests!
Нам нужен только failing test для decode layer поверх HLS.

**Цель**: Тест воспроизводит баг - при ABR switch с seek назад PCM samples пропускаются.

**Компоненты теста**:

1. **AbrTestServer** (из kithara-hls/tests/fixture.rs):
   - Уже существует
   - Возвращает текстовые сегменты: "V{variant}-SEG-{segment}:AAA..."
   - Поддерживает delay для триггера ABR

2. **MockDecoder** (создать в kithara-decode):
   ```rust
   impl Decoder for MockDecoder {
       fn decode_packet(&mut self, data: &[u8]) -> Result<Vec<f32>> {
           // Парсим "V0-SEG-1:AAA..." -> PCM samples
           // Pattern: [variant, segment, 0.0, 1.0, 2.0, ...]
           let text = String::from_utf8_lossy(data);
           let (variant, segment) = parse_segment_marker(&text)?;

           // Генерируем samples с уникальным pattern
           let mut samples = vec![variant as f32, segment as f32];
           samples.extend((0..100).map(|i| i as f32));
           Ok(samples)
       }
   }
   ```

3. **Pipeline<MockDecoder>**:
   - Использует MockDecoder вместо Symphonia
   - Работает с HLS source (AbrTestServer)
   - Возвращает PCM samples с variant/segment метаданными

4. **Сценарий теста**:
   ```
   1. Загружаем variant 0, segments 0-3
   2. Читаем 2KB (segments 0-1)
   3. ABR решает переключиться на variant 2
   4. Pipeline начинает загружать variant 2 с segment #4 (продолжение)
   5. Загружаем segments 4, 5, 6 из variant 2
   6. Decoder читает дальше, не находит нужный offset в variant 2
   7. Делается seek на segment #3 variant 2
   8. Segment #3 добавляется в BTreeMap: [3, 4, 5, 6]
   9. Читаем дальше
   ```

4. **Проверки**:
   ```rust
   // Проверяем что прочитанные байты последовательны
   fn verify_sequential_read(read_data: &[u8]) {
       // Разбираем по паттерну [variant, segment, data...]
       // Проверяем:
       // 1. Нет пропусков в segment_index
       // 2. Нет дублирования данных
       // 3. ABR switch произошел (меняется variant_id)
       // 4. После switch читаем из нового варианта
   }
   ```

**Файл теста**: `crates/kithara-hls/tests/variant_switch_sequential.rs`

**Ожидаемый результат**: Тест FAILS - обнаруживает пропуски/дублирование байтов.

---

### Фаза 2: Исправить код (GREEN)

**Решение**: Обновлять `first_media_segment` при добавлении более раннего сегмента.

**Изменения в `index.rs`**:

```rust
// VariantIndex::add()
fn add(&mut self, url: Url, len: u64, segment_index: usize, encryption: Option<EncryptionInfo>) {
    let key = if segment_index == usize::MAX {
        SegmentKey::Init
    } else {
        // Update first_media_segment if we're adding an earlier segment
        match self.first_media_segment {
            None => self.first_media_segment = Some(segment_index),
            Some(first) if segment_index < first => {
                tracing::debug!(
                    old_first = first,
                    new_first = segment_index,
                    "Updating first_media_segment (seek backward)"
                );
                self.first_media_segment = Some(segment_index);
            }
            _ => {}
        }
        SegmentKey::Media(segment_index)
    };

    // ... остальное без изменений
}
```

**Ожидаемый результат**: Тест PASSES - все байты читаются последовательно.

---

### Фаза 3: Расширить тесты (REFACTOR)

Добавить дополнительные сценарии:

1. **ABR Up → Down → Up**:
   - Variant 0 (128kbps) → Variant 2 (320kbps) → Variant 1 (256kbps) → Variant 2
   - Проверяем множественные переключения

2. **Seek вперед и назад**:
   - Загрузка начинается с segment #5
   - Seek на #2 (backward)
   - Seek на #8 (forward)
   - Seek на #0 (начало)

3. **Init segment**:
   - Вариант с INIT segment
   - Проверяем что INIT всегда первый (offset 0)

4. **Gap detection**:
   - Сценарий с пропущенными сегментами (4, 5, 7 - без 6)
   - Проверяем что `find()` возвращает None для gap

5. **Concurrent загрузка**:
   - Несколько вариантов загружаются параллельно
   - Проверяем изоляцию offset пространств

---

## Структура mock кода

### MockSegmentStream
```rust
struct MockSegmentStream {
    variants: HashMap<usize, Vec<MockSegment>>,
    load_order: Vec<(usize, usize)>,  // (variant, segment_index)
    current_pos: usize,
}

impl MockSegmentStream {
    fn with_scenario(scenario: TestScenario) -> Self {
        // scenario определяет:
        // - сколько вариантов
        // - размеры сегментов
        // - порядок загрузки (моделирует ABR)
    }

    fn next_segment(&mut self) -> Option<SegmentMeta> {
        // Возвращает сегменты в порядке load_order
    }
}
```

### Утилиты проверки PCM samples
```rust
fn verify_sequential_segments(samples: &[f32]) -> Result<(), SequentialError> {
    // MockDecoder генерирует pattern: [variant, segment, 0.0, 1.0, ...]
    // Каждый decoded packet начинается с [variant, segment]

    let mut last_segment = None;
    let mut i = 0;

    while i < samples.len() {
        // Первые два sample в каждом packet - метаданные
        if i + 1 >= samples.len() {
            break;
        }

        let variant = samples[i] as usize;
        let segment = samples[i + 1] as usize;

        if let Some((last_var, last_seg)) = last_segment {
            // Проверяем:
            // 1. Если тот же вариант - segment_index должен быть +1
            // 2. Если новый вариант - segment_index может быть любым (ABR switch)
            // 3. Нет дублирования (тот же variant+segment дважды)

            if variant == last_var && segment != last_seg + 1 {
                return Err(SequentialError::Gap {
                    expected: last_seg + 1,
                    got: segment,
                    variant,
                });
            }
        }

        last_segment = Some((variant, segment));

        // Skip to next packet (102 samples: 2 metadata + 100 data)
        i += 102;
    }

    Ok(())
}
```

---

## Статус выполнения

### ✅ Выполнено (неправильно - откат):
- ~~Создан интеграционный тест с `SourceReader`~~ - НЕ ПРАВИЛЬНО
- Тест проверял сырые байты, а не decode pipeline
- Не воспроизводит реальный баг

### ⏳ Следующий шаг:
**Фаза 0: Сделать decoder generic** - это критично!

### 🎯 Метрики успеха

✅ **Pipeline generic** - можно использовать MockDecoder
✅ **Интеграционный тест** - decode + HLS + ABR вместе
✅ **Тест воспроизводит баг** - FAILS без fix
✅ **Тест проходит с fix** - PASSES после исправления
✅ **PCM samples последовательны** - нет пропусков/дублирования
✅ **ABR переключения** корректно обрабатываются
✅ **Seek backward** не ломает decode
✅ **Не регрессия** на существующих тестах

---

## Текущий статус выполнения

### ✅ Фаза 0: Generic Decoder (ЗАВЕРШЕНО)
1. [x] Создать trait `Decoder` в `crates/kithara-decode/src/decoder.rs`
2. [x] Переместить Symphonia logic в `SymphoniaDecoder`
3. [x] Сделать `Pipeline<D: Decoder>` generic
4. [x] Создать `MockDecoder` для тестов (текстовые HLS сегменты)
5. [x] Создать `SimpleMockDecoder` для изолированных тестов (без I/O)
6. [x] Убедиться что существующие тесты проходят (все 12 тестов ✅)
7. [x] Проверить что SyncWorker/AsyncWorker работают (5 тестов ✅)
8. [x] Создать изолированные unit тесты Pipeline (4 теста ✅)

**Результаты Фазы 0:**
- ✅ trait Decoder работает
- ✅ Pipeline<D: Decoder> generic
- ✅ SymphoniaDecoder (production) - все тесты проходят
- ✅ SimpleMockDecoder (unit tests) - Pipeline работает изолированно
- ✅ MockDecoder (HLS integration) - декодирует текстовые сегменты
- ✅ SyncWorker базовая функциональность протестирована
- ⚠️  **ПРОБЛЕМА**: Pipeline + MockDecoder(HLS) - timeout при чтении через ring buffer

### ⏸️ Фаза 1: Failing Test (КРИТИЧНО - ТЕКУЩАЯ ЗАДАЧА)

**ЧТО НУЖНО СДЕЛАТЬ:**

Создать тест который:
1. ✅ Читает ВСЕ байты из HLS с ABR variant switch (через SourceReader)
2. ✅ Передает байты в MockDecoder в цикле
3. ✅ MockDecoder возвращает float samples с метаданными [variant, segment, ...]
4. ✅ Проверяет индекс КАЖДОГО байта в HLS stream
5. ✅ Проверяет индекс КАЖДОГО float sample от decoder
6. ✅ Обнаруживает баг: при ABR switch + seek backward пропускаются/дублируются байты
7. ✅ Test FAILS без fix
8. ✅ Test PASSES с fix

**ПОЧЕМУ НЕ НУЖЕН Pipeline:**
- Pipeline сложен для отладки (spawn_blocking, async/sync границы)
- Pipeline + MockDecoder(HLS via SyncReader) timeout (async/blocking mismatch)
- Баг в SegmentIndex, НЕ в Pipeline
- Тест должен быть простым и фокусированным

**ПРАВИЛЬНЫЙ ПОДХОД:**

```rust
#[tokio::test]
async fn test_abr_variant_switch_sequential_decode() {
    // 1. Setup HLS с ABR
    let server = AbrTestServer::new(...);
    let source = StreamSource::<Hls>::open(url, params).await?;
    let source_arc = Arc::new(source);

    // 2. SourceReader для чтения байтов
    let mut reader = SourceReader::new(source_arc);

    // 3. MockDecoder для декодирования
    let mut decoder = MockDecoder::new(&mut reader);

    // 4. Читаем ВСЕ байты и декодируем
    let mut all_bytes = Vec::new();
    let mut all_samples = Vec::new();

    loop {
        // Читаем chunk байтов
        let mut buf = vec![0u8; 256];
        let read = reader.read(&mut buf)?;
        if read == 0 { break; }

        all_bytes.extend_from_slice(&buf[..read]);

        // Декодируем
        if let Some(chunk) = decoder.next_chunk()? {
            all_samples.extend_from_slice(&chunk.pcm);
        }
    }

    // 5. ПРОВЕРКИ

    // A. Проверка КАЖДОГО байта - должны быть последовательными
    verify_sequential_bytes(&all_bytes)?;

    // B. Проверка КАЖДОГО float sample - должны быть последовательными
    // MockDecoder pattern: [variant, segment, 0.0, 1.0, ..., 99.0]
    verify_sequential_samples(&all_samples)?;

    // C. Проверка ABR switch произошел
    assert!(has_variant_switch(&all_samples));
}

fn verify_sequential_bytes(bytes: &[u8]) -> Result<()> {
    // Парсим "V{variant}-SEG-{segment}:" из каждого chunk'а байтов
    // Проверяем что segment индексы идут последовательно (0,1,2,3,...)
    // В пределах одного variant
    // FAIL если есть пропуск или дублирование
}

fn verify_sequential_samples(samples: &[f32]) -> Result<()> {
    // Каждый decoded chunk: [variant, segment, 0.0, 1.0, ..., 99.0]
    // Проверяем что segment индексы последовательны
    // FAIL если segment jump (например 0,1,2,5 - пропущен 3,4)
    // FAIL если дубликат (например 0,1,2,2 - segment 2 дважды)
}
```

**ОЖИДАЕМЫЙ РЕЗУЛЬТАТ:**

БЕЗ FIX:
```
test test_abr_variant_switch_sequential_decode ... FAILED
Error: Gap in segments: variant 2 jumped from segment 2 to segment 4
  (пропущен segment 3 из-за неправильного first_media_segment)
```

С FIX (first_media_segment обновляется):
```
test test_abr_variant_switch_sequential_decode ... ok
```

**ТЕКУЩИЙ СТАТУС:**

- [ ] Создать `test_abr_variant_switch_sequential_decode`
- [ ] MockDecoder работает через `&mut Read` (не через SyncReader!)
- [ ] Verify functions проверяют КАЖДЫЙ элемент
- [ ] Запустить БЕЗ fix → FAIL
- [ ] Запустить С fix → PASS

**БЛОКЕРЫ УБРАНЫ:**
- ❌ НЕ используем Pipeline (убран async/blocking mismatch)
- ❌ НЕ используем SyncReader в spawn_blocking
- ✅ Простой sync test: SourceReader → MockDecoder → verify

### Фаза 2: Fix (GREEN) - ГОТОВО К ВЫПОЛНЕНИЮ
10. [x] Fix уже применен: `VariantIndex::add()` обновляет `first_media_segment`
11. [ ] Запустить существующие тесты ABR - убедиться что PASS

### Фаза 3: Расширение (REFACTOR)
12. [ ] Добавить расширенные сценарии (ABR up/down, множественные switches)
13. [ ] Запустить все тесты workspace
14. [ ] Проверить на реальном HLS stream (example hls_decode)

---

## Риски и ограничения

**Риск 1**: Gap detection может срабатывать при легитимных seek backward
- **Митигация**: Gap detection должен учитывать что сегменты могут быть не по порядку

**Риск 2**: Производительность при частых seek
- **Митигация**: BTreeMap.insert() всё равно O(log n), порядок не важен

**Риск 3**: Concurrent доступ к first_media_segment
- **Митигация**: Уже защищено RwLock в SegmentIndex

---

## Альтернативные решения (отклонены)

### ❌ Base offset подход
- Пытались сделать непрерывное пространство offsets через base_offset
- Проблема: HLS сегменты - дискретные файлы, не непрерывный поток
- Decoder должен читать каждый сегмент с начала, а не с середины

### ❌ Переделать на Vec вместо BTreeMap
- BTreeMap нужен для автоматической сортировки по segment_index
- Vec потребует ручной сортировки и поиска

### ✅ Обновлять first_media_segment (выбрано)
- Минимальные изменения
- Правильно отражает реальное состояние
- Совместимо с существующей логикой
