# Progressive Download Architecture Analysis

Дата: 2026-02-01
Статус: **Анализ проблем**
Следующий шаг: Согласование решений → TDD тесты → Имплементация

## Проблема

Текущий код kithara (после revert) имеет проблемы с seek. Stash с TDD тестами сломал HLS. Корневая причина: **архитектура трейтов не подходит для progressive download сценариев**.

## Обнаруженные сценарии

Из production bugs и stash тестов:

### File Progressive Download
1. **Early stream close**: HTTP stream закрывается на 512KB из 1MB файла
2. **Partial cache resume**: App crash → reopen → seek beyond cached

### HLS Streaming
3. **ABR variant switch**: Переключение между вариантами меняет byte offsets
4. **Time-based seek**: Seek по времени (нет duration tracking в segments)
5. **Seek to unloaded segment**: Seek далеко вперёд, segment не загружен
6. **Multiple backward seeks**: Forward → back → back снова → EOF
7. **Downloader EOF handling**: Downloader должен остановиться на EOF
8. **HLS → File track switch**: Переключение треков, старый source должен остановиться

### Общая проблема: Writer auto-commit (File + HLS)

**Writer используется обоими:**
- **File**: один Writer на весь файл (`FileDownloader` wraps `Writer`)
- **HLS**: один Writer per сегмент (`FetchManager.start_fetch()` создаёт `Writer`)

**Один и тот же баг** (`writer.rs:143-148`):
```rust
let Some(next) = next else {
    // Stream ended → auto-commit → WRONG for partial downloads
    if let Err(e) = res.commit(Some(offset)) { ... }
    yield Ok(WriterItem::Completed { total_bytes: offset });
    return;
};
```

**Как это проявляется:**

| | File | HLS |
|--|------|-----|
| **Writer per** | Весь файл | Каждый сегмент |
| **Что происходит** | Stream closes at 512KB of 1MB → commit(512KB) | Network drops mid-segment → commit(partial) |
| **Результат** | Resource committed с wrong final_len | Segment marked committed но данные неполные |
| **Seek fails** | wait_range returns Eof (512KB < seek target) | read_at reads corrupt data from partial segment |

**Решение на уровне Writer исправит ОБА:**
- Writer: StreamEnded вместо Completed + no auto-commit
- Caller (FileDownloader / FetchManager) проверяет total_bytes vs expected и решает commit или нет

## Архитектурные проблемы

### 1. Resource State Ambiguity

**Текущее состояние:**
```rust
enum ResourceStatus {
    Active,
    Committed { final_len: u64 },
    Failed { reason: String },
}
```

**Проблема:**
- `Active` не показывает: сколько скачано, сколько ожидается
- Невозможно отличить:
  - Partial uncommitted: 512KB downloaded, 1MB expected
  - Complete but not committed: 512KB downloaded, 512KB expected
- После reopen нет информации об expected_total

**Предлагаемое решение:**
```rust
enum ResourceStatus {
    Active {
        downloaded: u64,      // Bytes written so far
        expected: Option<u64>, // Expected total from Content-Length
    },
    Committed {
        final_len: u64,
    },
    Failed {
        reason: String,
    },
}
```

**Преимущества:**
- Явно видно состояние partial download
- Можно принять решение: commit или continue
- При reopen можем определить что файл partial

---

### 2. Writer::Completed vs Committed

**Текущий API:**
```rust
enum WriterItem {
    ChunkWritten { offset: u64, len: usize },
    /// Download completed, resource committed. <-- НЕПРАВИЛЬНО
    Completed { total_bytes: u64 },
}
```

**Проблема:**
- Документация говорит "resource committed"
- Но stream ended ≠ file complete
- HTTP stream может закрыться рано (сетевой обрыв)
- Кто должен вызывать commit?

**Предлагаемое решение:**
```rust
enum WriterItem {
    ChunkWritten { offset: u64, len: usize },
    /// Stream ended at this byte offset.
    /// Does NOT mean resource is committed!
    /// Caller (Downloader) decides to commit based on expected_total.
    StreamEnded { total_bytes: u64 },
}
```

**Caller logic (FileDownloader):**
```rust
Ok(WriterItem::StreamEnded { total_bytes }) => {
    if total_bytes >= self.expected_total {
        self.resource.commit(Some(total_bytes)); // Complete
        return false; // Exit downloader
    } else {
        // Partial - DON'T commit
        self.sequential_ended = true;
        return true; // Continue for on-demand
    }
}
```

**Caller logic (HLS FetchManager):**
```rust
Ok(WriterItem::StreamEnded { total_bytes }) => {
    // HLS: проверить что сегмент загружен полностью
    // Content-Length известен из HTTP response
    if total_bytes >= expected_segment_len {
        // Caller (FetchManager) вызывает commit вручную
        resource.commit(Some(total_bytes))?;
        total = total_bytes;
        break;
    } else {
        // Partial segment — НЕ добавлять в SegmentIndex!
        // Retry или skip
        return Err(HlsError::PartialSegment { ... });
    }
}
```

**Преимущества:**
- Явная семантика: stream ended, не обязательно committed
- **File**: Downloader решает commit или on-demand mode
- **HLS**: FetchManager решает commit или retry/skip
- Оба используют один и тот же Writer, fix на одном уровне

---

### 3. Downloader Lifecycle: Sequential → On-Demand

**Текущий API:**
```rust
trait Downloader {
    async fn step(&mut self) -> bool; // false = done, exit
}
```

**Проблема:**
- Sequential download ends → step() returns false → Backend exit
- Нельзя обрабатывать on-demand Range requests после окончания sequential
- В stash это решалось через `sequential_ended` флаг

**Предлагаемое решение:**

Option A: Downloader поддерживает два режима явно
```rust
impl FileDownloader {
    async fn step(&mut self) -> bool {
        // 1. Check for on-demand Range request
        if let Some(range) = self.check_pending_range() {
            self.fetch_range(range).await;
            return true; // Continue
        }

        // 2. Sequential download (if not ended)
        if self.sequential_ended {
            // Wait for on-demand request or cancel
            self.wait_for_range_request().await;
            return true; // Keep running
        }

        // 3. Normal sequential chunk processing
        // ...
    }
}
```

Option B: Separate trait methods
```rust
trait Downloader {
    async fn step_sequential(&mut self) -> DownloadOutcome;
    async fn step_on_demand(&mut self) -> bool;
}

enum DownloadOutcome {
    Continue,
    SequentialComplete, // Switch to on-demand mode
    AllComplete,        // Exit
}
```

**Решение в stash:** Option A (один метод, два режима)

**Преимущества:**
- Downloader продолжает работать после sequential end
- Может обрабатывать on-demand Range requests
- Поддержка partial download + seek beyond

---

### 4. Source::len() Semantics

**Текущая реализация:**
```rust
impl Source for FileSource {
    fn len(&self) -> Option<u64> {
        self.resource.len() // committed length
    }
}
```

**Проблема:**
- Для partial uncommitted: resource.len() = None
- Decoder (Symphonia) не знает размер файла
- Duration calculation fails
- Seeks вычисляются неправильно

**Предлагаемое решение:**
```rust
impl Source for FileSource {
    fn len(&self) -> Option<u64> {
        // Priority: expected_total > committed_len
        self.expected_total.or_else(|| self.resource.len())
    }
}
```

**Преимущества:**
- Decoder видит ожидаемый размер файла (из Content-Length)
- Duration calculation работает правильно
- Time-based seeks вычисляются правильно
- Даже для partial downloads

---

### 5. wait_range() Blocking on Partial

**Текущее поведение:**
```rust
fn wait_range(&self, range: Range<u64>) -> WaitOutcome {
    // Blocks until range is available
    // For partial uncommitted: blocks forever if range > downloaded
}
```

**Проблема:**
- Reader seeks to 700KB (partial file has 512KB)
- Source calls wait_range(700KB..710KB)
- Resource blocks waiting for data
- Sequential downloader has exited
- **Deadlock**

**Решения:**

Option A: wait_range() returns new outcome
```rust
enum WaitOutcome {
    Ready,
    Eof,
    NeedsFetch, // <-- NEW: range not available, need on-demand
}
```

Option B: Source checks before calling wait_range()
```rust
impl FileSource {
    fn wait_range(&mut self, range: Range<u64>) -> WaitOutcome {
        let downloaded = self.progress.download_pos();

        if range.start >= downloaded {
            // Request on-demand fetch instead of waiting
            self.shared.request_range(range.clone());
            self.shared.wait_for_fetch();
            return WaitOutcome::Ready;
        }

        // Normal wait on resource
        self.resource.wait_range(range)
    }
}
```

**Решение в stash:** Option B (Source checks download_pos first)

**Преимущества:**
- Нет deadlock
- On-demand fetch запускается автоматически
- ResourceExt::wait_range() остаётся простым

---

### 6. On-Demand Request Mechanism

**Вопрос:** Как Source сигнализирует Downloader о запросе Range?

**Варианты:**

A) Trait method:
```rust
trait Source {
    fn request_range(&mut self, range: Range<u64>);
}
```

B) SharedState (stash approach):
```rust
struct SharedState {
    pending_ranges: Mutex<VecDeque<Range<u64>>>,
    range_requested: Notify,
}

impl FileSource {
    fn wait_range(&mut self, range: Range<u64>) {
        if range.start >= downloaded {
            self.shared.request_range(range);
            self.shared.wait();
        }
    }
}
```

C) Channel:
```rust
struct FileSource {
    range_tx: mpsc::Sender<Range<u64>>,
}
```

**Решение в stash:** Option B (SharedState)

**Вопрос для обсуждения:**
- SharedState coupling Source ↔ Downloader implementation
- Trait method чище, но как Downloader получает запрос?
- Принять SharedState как standard pattern?

---

### 7. Partial Download Tracking via Index

**Проблема:**
- App crashes на 512KB of 1MB download
- При reopen: файл существует (512KB data)
- OpenMode::Auto treats as committed
- Информация о expected_total потеряна

**Предлагаемое решение:**

Использовать существующий паттерн индексов (`PinsIndex`, `LruIndex`):
добавить `DownloadIndex` по аналогии — `StorageResource` + bincode.

**По аналогии с существующими индексами:**
```rust
// По образцу PinsIndex / LruIndex
pub struct DownloadIndex {
    res: StorageResource, // _index/downloads.bin (bincode, не JSON!)
}

#[derive(serde::Serialize, serde::Deserialize)]
struct DownloadIndexFile {
    version: u32,
    entries: Vec<DownloadEntry>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct DownloadEntry {
    rel_path: String,         // ResourceKey path
    expected_total: u64,      // Content-Length
    downloaded: u64,           // Bytes written
    etag: Option<String>,     // Cache validation
}
```

**Assets trait расширение:**
```rust
trait Assets {
    // Уже есть:
    fn open_pins_index_resource(&self) -> AssetsResult<StorageResource>;
    fn open_lru_index_resource(&self) -> AssetsResult<StorageResource>;

    // Добавить:
    fn open_download_index_resource(&self) -> AssetsResult<StorageResource>;
}
```

**DiskAssetStore:**
```rust
fn download_index_path(&self) -> PathBuf {
    self.root_dir.join("_index").join("downloads.bin")
}
```

**Open logic (в FileStreamState::create):**
```rust
let download_index = DownloadIndex::open(&store)?;
let state = download_index.load()?;

if let Some(entry) = state.get(rel_path) {
    // Partial download found — resume
    let resource = store.open_resource(&key)?; // ReadWrite mode
    // expected_total из индекса, downloaded из entry
    resume_download(resource, entry.expected_total, entry.downloaded);
} else if resource.status() == Committed {
    // Complete file
} else {
    // New download
}
```

**Lifecycle:**
- На старте download: `download_index.insert(rel_path, expected_total, 0)`
- Периодически при download progress: `download_index.update_downloaded(rel_path, pos)`
- При commit (complete): `download_index.remove(rel_path)` (committed файл = полный)
- При reopen: `download_index.load()` → знаем expected_total для partial

**Преимущества:**
- Тот же паттерн что PinsIndex/LruIndex — единообразие
- bincode формат (быстрый, компактный)
- StorageResource для persistence
- Знаем expected_total при reopen
- Cache validation через ETag
- Нет отдельных .partial файлов — всё в одном индексе

---

### 8. HLS: Segment Duration Tracking

**Текущий SegmentEntry:**
```rust
struct SegmentEntry {
    url: String,
    byte_range: Range<u64>,
    // Missing: duration!
}
```

**Проблема:**
- Time-based seek: decoder.seek_to_time(36 seconds)
- Как найти segment который содержит 36s?
- Нет duration → нет mapping time → segment

**Предлагаемое решение:**
```rust
struct SegmentEntry {
    url: String,
    byte_range: Range<u64>,
    duration: Option<Duration>,         // From #EXTINF
    timestamp_start: Option<Duration>,  // Cumulative
}

impl HlsSource {
    fn find_segment_by_time(&self, time: Duration) -> Option<&SegmentEntry> {
        self.segments.iter().find(|seg| {
            let start = seg.timestamp_start?;
            let end = start + seg.duration?;
            start <= time && time < end
        })
    }
}
```

**Parsing:**
```
#EXTINF:4.0
segment0.ts

#EXTINF:4.0
segment1.ts
```

→
```
Segment 0: duration=4s, timestamp_start=0s
Segment 1: duration=4s, timestamp_start=4s
```

**Преимущества:**
- Time-based seek работает
- Можно skip to specific time без загрузки всех segments
- Decoder видит правильную duration

---

### 9. HLS: ABR Variant Switch & Virtual Byte Space

**Проблема:**
```
Variant 0 (AAC-LC, ~50KB/segment):
  Segment 0: bytes 0-50000
  Segment 1: bytes 50000-100000
  Segment 2: bytes 100000-150000

ABR switch to Variant 3 (FLAC, ~700KB/segment):
  Segment 3: bytes 150000-850000  <-- WRONG physical offset!
```

User seeks back to segment 1:
- Decoder seeks to byte 50000
- But current variant is 3, not 0!
- Format mismatch, seek fails

**Root cause:**
- byte_range физические, зависят от реального размера сегментов
- После ABR switch offsets меняются
- Decoder confused

**Предлагаемое решение: Virtual Byte Space**

```rust
const VIRTUAL_SEGMENT_SIZE: u64 = 1_000_000; // 1MB per segment

fn segment_virtual_range(index: usize) -> Range<u64> {
    let start = index as u64 * VIRTUAL_SEGMENT_SIZE;
    start..(start + VIRTUAL_SEGMENT_SIZE)
}
```

**Mapping:**
```
Virtual space (Decoder sees):
  Segment 0: bytes 0-1000000
  Segment 1: bytes 1000000-2000000
  Segment 2: bytes 2000000-3000000
  // Same for all variants!

Physical space (StorageResource):
  Variant 0, Segment 0: resource_v0_seg0 (50KB actual)
  Variant 3, Segment 3: resource_v3_seg3 (700KB actual)
```

**HLS Source maps:**
```rust
impl Source for HlsSource {
    fn read_at(&mut self, virtual_offset: u64, buf: &mut [u8]) -> usize {
        // 1. Map: virtual_offset → segment_index
        let seg_idx = (virtual_offset / VIRTUAL_SEGMENT_SIZE) as usize;
        let offset_in_seg = virtual_offset % VIRTUAL_SEGMENT_SIZE;

        // 2. Get current segment entry
        let entry = self.get_segment(seg_idx)?;

        // 3. Read from actual StorageResource
        entry.resource.read_at(offset_in_seg, buf)
    }
}
```

**Преимущества:**
- ABR switch не меняет virtual offsets
- Decoder seeks работают корректно
- Segment boundaries predictable

---

## Резюме изменений

| Компонент | Текущее состояние | Предлагаемое изменение | Scope | Приоритет |
|-----------|-------------------|------------------------|-------|-----------|
| WriterItem | Completed (auto-commit) | → StreamEnded (no commit) | **File + HLS** | 🔴 Critical |
| ResourceStatus | Active/Committed/Failed | + downloaded, expected fields | **File + HLS** | 🔴 Critical |
| Source::len() | Returns committed_len | Returns expected_total first | **File** | 🔴 Critical |
| wait_range() deadlock | Blocks forever | Check download_pos first | **File** | 🔴 Critical |
| Downloader lifecycle | Exit after sequential | Continue for on-demand | **File** | 🔴 Critical |
| On-demand mechanism | Нет standard way | SharedState pattern | **File** | 🟡 Important |
| DownloadIndex | Нет persistence | bincode index (как PinsIndex) | **File + HLS** | 🟡 Important |
| HLS partial segment | No check before index add | Check total_bytes vs expected | **HLS** | 🟡 Important |
| HLS duration tracking | Нет duration in SegmentEntry | Add duration, timestamp | **HLS** | 🟠 Medium |
| HLS virtual byte space | Physical offsets | Virtual 1MB/segment | **HLS** | 🟠 Medium |

## Следующие шаги

### Phase 1: Writer fix (File + HLS)
Это единственное изменение которое затрагивает оба протокола.

1. **TDD RED**: Writer test — stream ends, resource NOT committed
2. **GREEN**: Writer: убрать auto-commit, yield StreamEnded
3. **Fix callers**:
   - FileDownloader: проверить total_bytes vs Content-Length → commit или on-demand
   - HLS FetchManager: проверить total_bytes vs expected → commit или retry

### Phase 2: File progressive download
Решает: early stream close, partial cache resume, on-demand Range requests.

4. **TDD RED**: File seek beyond partial download → deadlock
5. **GREEN**: FileDownloader on-demand mode (sequential_ended + SharedState)
6. **TDD RED**: Source::len() returns None for partial
7. **GREEN**: Source::len() returns expected_total
8. **TDD RED**: wait_range() deadlocks on partial
9. **GREEN**: FileSource checks download_pos before wait_range()

### Phase 3: DownloadIndex (File + HLS)
Решает: partial cache resume после app restart.

10. **TDD RED**: Reopen partial file → treated as committed
11. **GREEN**: DownloadIndex (bincode, как PinsIndex) tracks partial downloads
12. **Assets trait**: добавить open_download_index_resource()

### Phase 4: HLS partial segment handling
Решает: corrupt segments from network drops.

13. **TDD RED**: HLS partial segment in index → read corrupt data
14. **GREEN**: FetchManager checks total_bytes before adding to SegmentIndex

### Phase 5: HLS-specific improvements (later)
15. Duration tracking in SegmentEntry
16. Virtual byte space for ABR

## Открытые вопросы

1. **On-demand request mechanism:** SharedState vs trait method?
2. **Virtual byte space size:** 1MB фиксированный или configurable?
3. **ResourceStatus breaking change:** Как мигрировать existing code?
