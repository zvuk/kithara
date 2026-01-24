# Kithara Optimization Roadmap

**Цель:** High-performance плеер с минимальным потреблением памяти
**Дата:** 2026-01-25

---

## Sprint 1: Critical Fixes (1-2 дня)

### Задача 1.1: Ограничить buffered_chunks в HlsSourceAdapter ⚠️⚠️⚠️
**Приоритет:** CRITICAL
**Экономия памяти:** 30 MB per stream
**Файл:** `crates/kithara-hls/src/worker/adapter.rs:164`

**Текущий код:**
```rust
if !fetch.data.is_empty() {
    self.buffered_chunks.lock().push(fetch.data);
}
```

**Оптимизация:**
```rust
const MAX_BUFFERED_CHUNKS: usize = 5;

if !fetch.data.is_empty() {
    let mut chunks = self.buffered_chunks.lock();

    // Enforce maximum size
    while chunks.len() >= MAX_BUFFERED_CHUNKS {
        chunks.remove(0);  // Drop oldest chunk
    }

    chunks.push(fetch.data);
}
```

**Тесты:**
- Verify chunks.len() never exceeds 5
- Verify playback continues after limit reached
- Measure memory usage before/after

**Effort:** 10 lines
**Risk:** Low

---

### Задача 1.2: Zero-copy init+media композиция ⚠️⚠️⚠️
**Приоритет:** CRITICAL
**Экономия:** 2 MB allocation/copy per segment, 1-3ms latency
**Файл:** `crates/kithara-hls/src/worker/source.rs:266-269`

**Текущий код:**
```rust
let mut combined = Vec::with_capacity(init_bytes.len() + media_bytes.len());
combined.extend_from_slice(&init_bytes);
combined.extend_from_slice(&media_bytes);
Bytes::from(combined)
```

**Оптимизация (Option A - Bytes::chain):**
```rust
// Note: Requires HlsMessage to handle chained Bytes or iterator
// This may require refactoring HlsMessage to use Bytes instead of single blob
let combined = init_bytes.chain(media_bytes);
```

**Оптимизация (Option B - BytesMut):**
```rust
use bytes::BytesMut;

let mut combined = BytesMut::with_capacity(init_bytes.len() + media_bytes.len());
combined.put(init_bytes);
combined.put(media_bytes);
combined.freeze()
```

**Тесты:**
- Verify playback works with new format
- Benchmark copy time reduction
- Verify no regression in memory usage

**Effort:** 5-20 lines (depending on option)
**Risk:** Medium (requires careful testing)

---

### Задача 1.3: Увеличить chunk channel capacity до 8 ⚠️⚠️
**Приоритет:** CRITICAL
**Экономия:** Eliminates worker blocking, enables prefetch
**Файл:** `crates/kithara-hls/src/source.rs:169`

**Текущий код:**
```rust
let (chunk_tx, chunk_rx) = kanal::bounded_async(2);
```

**Оптимизация:**
```rust
const HLS_CHUNK_CHANNEL_CAPACITY: usize = 8;
let (chunk_tx, chunk_rx) = kanal::bounded_async(HLS_CHUNK_CHANNEL_CAPACITY);
```

**Тесты:**
- Verify worker doesn't block during fast downloads
- Measure latency improvement
- Verify memory usage stays bounded

**Effort:** 1-3 lines
**Risk:** Low

---

## Sprint 2: High Priority (2-3 дня)

### Задача 2.1: Arc<OnceCell> для ProcessedResource ⚠️⚠️
**Приоритет:** HIGH
**Экономия:** Eliminates 25-70ms lock contention
**Файл:** `crates/kithara-assets/src/processing.rs`

**Текущая структура:**
```rust
struct ProcessedResource {
    buffer: Arc<Mutex<Option<Bytes>>>,
}

async fn ensure_processed(&self) -> Result<Bytes> {
    let mut buffer = self.buffer.lock().await;  // Lock held during I/O ⚠️
    let raw = self.inner.read().await;
    let processed = (self.process)(raw, self.ctx.clone()).await;
    *buffer = Some(processed.clone());
    Ok(processed)
}
```

**Оптимизация:**
```rust
use tokio::sync::OnceCell;

struct ProcessedResource {
    buffer: Arc<OnceCell<Bytes>>,
    inner: Arc<dyn StreamingResourceExt>,
    process: Arc<ProcessFn<Ctx>>,
    ctx: Ctx,
}

async fn ensure_processed(&self) -> Result<Bytes> {
    self.buffer
        .get_or_try_init(|| async {
            let raw = self.inner.read().await;
            (self.process)(raw, self.ctx.clone()).await
        })
        .await
        .map(|b| b.clone())
}
```

**Изменения:**
1. Replace `Mutex<Option<Bytes>>` with `OnceCell<Bytes>`
2. Use `get_or_try_init` for lock-free initialization
3. Subsequent reads are lock-free Arc::clone

**Тесты:**
- Verify concurrent reads don't block
- Benchmark lock contention reduction
- Verify no data races

**Effort:** 30 lines
**Risk:** Low

---

### Задача 2.2: Pool prefetch buffers ⚠️
**Приоритет:** HIGH
**Экономия:** 95-98% fewer allocations (640 KB/s → 10-30 KB/s)
**Файл:** `crates/kithara-stream/src/source.rs:169-255`

**Текущий код:**
```rust
async fn fetch_next(&mut self) -> Fetch<ByteChunk> {
    let mut buf = vec![0u8; chunk_size];  // Fresh allocation
    let bytes_read = self.src.read_at(pos, &mut buf).await?;
    // ...
}
```

**Оптимизация:**
```rust
use kithara_bufpool::SharedPool;

struct BytePrefetchSource<S> {
    pool: SharedPool<32, Vec<u8>>,
    // ... existing fields
}

impl<S> BytePrefetchSource<S> {
    pub fn new(src: S, chunk_size: usize, pool: SharedPool<32, Vec<u8>>) -> Self {
        // ... initialize with pool
    }

    async fn fetch_next(&mut self) -> Fetch<ByteChunk> {
        let mut buf = self.pool.get_with(|b| {
            b.clear();
            b.resize(chunk_size, 0);
        });

        let bytes_read = self.src.read_at(pos, &mut buf).await?;
        buf.truncate(bytes_read);

        // Ownership transfers to ByteChunk, returned to pool when dropped
        Fetch::new(ByteChunk {
            file_pos: pos,
            data: buf.into_inner(),
        })
    }
}
```

**Тесты:**
- Measure allocation rate reduction
- Verify no memory leaks
- Benchmark performance impact

**Effort:** 20 lines
**Risk:** Low

---

### Задача 2.3: Pool PcmChunk allocations ⚠️
**Приоритет:** HIGH
**Экономия:** 95-98% fewer allocations (20 KB/s - 1.6 MB/s → minimal)
**Файл:** `crates/kithara-decode/src/symphonia_mod/decoder.rs:293`

**Аналогично задаче 2.2 - использовать SharedPool для pcm buffers**

**Effort:** 20 lines
**Risk:** Low

---

### Задача 2.4: Compact JSON (убрать pretty-print) ⚠️
**Приоритет:** HIGH
**Экономия:** 30% disk usage, 1-2ms per operation
**Файлы:**
- `crates/kithara-assets/src/index/lru.rs`
- `crates/kithara-assets/src/index/pin.rs`

**Текущий код:**
```rust
let bytes = serde_json::to_vec_pretty(&file)?;
```

**Оптимизация:**
```rust
let bytes = serde_json::to_vec(&file)?;  // Remove _pretty
```

**Тесты:**
- Verify backward compatibility (can read old pretty-printed files)
- Measure file size reduction
- Benchmark serialize/deserialize time

**Effort:** 2 lines
**Risk:** Very low

---

## Sprint 3: Medium Priority (3-5 дней)

### Задача 3.1: Binary format для indexes (bincode) ⚠️
**Приоритет:** MEDIUM
**Экономия:** 50-70% disk usage, faster serialize/deserialize
**Файлы:** `crates/kithara-assets/src/index/*.rs`

**Добавить зависимость:**
```toml
[workspace.dependencies]
bincode = "1.3"
```

**Миграция:**
```rust
// Support both formats for backward compatibility
async fn load(&self) -> Result<LruState> {
    let bytes = self.res.read().await?;

    // Try bincode first
    if let Ok(state) = bincode::deserialize(&bytes) {
        return Ok(state);
    }

    // Fallback to JSON
    serde_json::from_slice(&bytes).map_err(Into::into)
}

async fn store(&self, state: &LruState) -> Result<()> {
    let bytes = bincode::serialize(state)?;
    self.res.write(&bytes).await
}
```

**Тесты:**
- Test migration from JSON to bincode
- Verify backward compatibility
- Benchmark size and speed improvements

**Effort:** 50 lines
**Risk:** Medium (requires migration strategy)

---

### Задача 3.2: Batch JSON updates ⚠️
**Приоритет:** MEDIUM
**Экономия:** 10-50x fewer disk writes
**Файлы:** `crates/kithara-assets/src/lease.rs`, `crates/kithara-assets/src/evict.rs`

**Стратегия:**
1. **In-memory dirty flag:**
   ```rust
   struct LeaseAssets {
       pins: Arc<Mutex<HashSet<String>>>,
       pins_dirty: Arc<AtomicBool>,
       // ...
   }
   ```

2. **Debounced flush:**
   ```rust
   // Flush every 1 second or on 10 changes
   async fn flush_pins_if_needed(&self) {
       if self.pins_dirty.load(Ordering::Acquire) {
           self.persist_pins().await;
           self.pins_dirty.store(false, Ordering::Release);
       }
   }
   ```

3. **Graceful shutdown:**
   ```rust
   impl Drop for LeaseAssets {
       fn drop(&mut self) {
           // Ensure final flush
           tokio::spawn(async {
               self.flush_pins_if_needed().await;
           });
       }
   }
   ```

**Тесты:**
- Verify writes are batched
- Verify data persists on shutdown
- Measure disk write reduction

**Effort:** 100 lines
**Risk:** Medium (requires careful Drop handling)

---

### Задача 3.3: Fix LeaseGuard async drop ⚠️
**Приоритет:** MEDIUM
**Экономия:** Prevents races, unbounded spawn accumulation
**Файл:** `crates/kithara-assets/src/lease.rs:323-338`

**Текущий код:**
```rust
impl Drop for LeaseGuard {
    fn drop(&mut self) {
        let owner = self.owner.clone();
        let asset_root = self.asset_root.clone();
        tokio::spawn(async move {
            owner.unpin_best_effort(&asset_root).await;
        });
    }
}
```

**Оптимизация (Option A - Lazy unpin):**
```rust
// Don't persist immediately, defer to next eviction check
impl Drop for LeaseGuard {
    fn drop(&mut self) {
        let mut pins = self.owner.pins.blocking_lock();
        pins.remove(&self.asset_root);
        self.owner.pins_dirty.store(true, Ordering::Release);
        // Actual persist happens in background flush task
    }
}
```

**Оптимизация (Option B - Structured concurrency):**
```rust
struct LeaseAssets {
    unpin_tasks: Arc<Mutex<JoinSet<()>>>,
}

impl Drop for LeaseGuard {
    fn drop(&mut self) {
        let handle = tokio::spawn(async {
            owner.unpin_best_effort(&asset_root).await;
        });
        self.owner.unpin_tasks.lock().spawn(handle);
    }
}

// In LeaseAssets::shutdown()
async fn shutdown(&self) {
    let mut tasks = self.unpin_tasks.lock();
    while let Some(res) = tasks.join_next().await {
        // Wait for all unpins to complete
    }
}
```

**Effort:** 30-50 lines
**Risk:** Medium

---

### Задача 3.4: Clear init_segments_cache on variant switch
**Приоритет:** MEDIUM
**Экономия:** 30-50 KB per variant
**Файл:** `crates/kithara-hls/src/worker/source.rs:354`

**Текущий код:**
```rust
if new_variant != current_variant {
    self.sent_init_for_variant.remove(&new_variant);
    self.current_variant = new_variant;
}
```

**Оптимизация:**
```rust
if new_variant != current_variant {
    // Clear old variant's init segment
    self.init_segments_cache.remove(&current_variant);
    self.sent_init_for_variant.remove(&new_variant);
    self.current_variant = new_variant;
}
```

**Effort:** 1 line
**Risk:** Very low

---

## Sprint 4: Low Priority / Future (Optional)

### Задача 4.1: Connection pooling (optional) ⚪
**Приоритет:** LOW
**Trade-off:** 50-200ms faster repeated requests vs higher memory
**Файл:** `crates/kithara-net/src/client.rs:80-103`

**Текущий код:**
```rust
.pool_max_idle_per_host(0)  // DISABLED
```

**Оптимизация:**
```rust
.pool_max_idle_per_host(8)  // Enable for same-host requests
.pool_idle_timeout(Duration::from_secs(90))
```

**Effort:** 1-2 lines
**Risk:** Low
**Note:** Only beneficial for same-host repeated requests

---

### Задача 4.2: SIMD sample conversion ⚪
**Приоритет:** LOW
**Экономия:** 2-4x faster conversion
**Файл:** `crates/kithara-decode/src/resampler/processor.rs:288`

**Стратегия:**
```rust
#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::*;

#[cfg(target_arch = "x86_64")]
unsafe fn interleave_simd_avx2(planar: &[Vec<f32>]) -> Vec<f32> {
    // Use _mm256_load_ps, _mm256_shuffle_ps, etc.
    // Process 8 f32 at a time
}
```

**Effort:** 100+ lines
**Risk:** High (unsafe code, platform-specific)

---

### Задача 4.3: Parallel segment downloads ⚪
**Приоритет:** LOW
**Экономия:** Reduces buffer gaps during variant switches
**Файл:** `crates/kithara-hls/src/worker/source.rs`

**Стратегия:**
- Spawn background task to download N+1 segment
- Store in-progress downloads in HashMap
- Cancel stale downloads on seek

**Effort:** 200+ lines
**Risk:** High (complex coordination)

---

## Измеримые метрики для мониторинга

### Before Optimization Baseline

```bash
# Memory usage (RSS)
ps -o rss= -p $(pidof hls_decode)

# Allocation rate
cargo build --release
RUSTFLAGS="-C instrument-coverage" cargo run --release --example hls_decode
# Count allocations with jemalloc profiling

# Latency to first sample
# Add timing instrumentation:
let start = Instant::now();
let _pcm = pipeline.pcm_rx().recv().await;
eprintln!("Time to first PCM: {:?}", start.elapsed());
```

### Target Metrics

| Metric | Current | Target | Improvement |
|--------|---------|--------|-------------|
| **Peak RSS** | 120 MB | 30 MB | 75% reduction |
| **Allocation rate** | 1-10 MB/s | 100-500 KB/s | 90-95% reduction |
| **Cold start latency** | 500-2500ms | 350-2000ms | 20-30% reduction |
| **buffered_chunks size** | 40+ MB | 10 MB | 75% reduction |
| **JSON disk I/O** | 25 KB/op | 8-12 KB/op | 50-70% reduction |

---

## Testing Plan

### Unit Tests

```rust
// crates/kithara-hls/tests/adapter_limits.rs
#[tokio::test]
async fn test_buffered_chunks_limit() {
    let adapter = setup_adapter();

    // Send 20 chunks
    for i in 0..20 {
        adapter.receive_chunk(mock_chunk(i)).await;
    }

    // Verify max 5 retained
    let chunks = adapter.buffered_chunks.lock();
    assert!(chunks.len() <= 5);
    assert_eq!(chunks[0].segment_index, 15); // Oldest kept is 15
}
```

### Integration Tests

```rust
// crates/kithara-decode/tests/allocation_rate.rs
#[tokio::test]
async fn test_allocation_rate_with_pooling() {
    let before_count = allocator::allocation_count();

    decode_10_seconds_of_audio().await;

    let after_count = allocator::allocation_count();
    let rate = (after_count - before_count) / 10.0; // per second

    assert!(rate < 100.0, "Too many allocations: {}/s", rate);
}
```

### Performance Benchmarks

```rust
// crates/kithara-hls/benches/segment_fetch.rs
use criterion::{black_box, criterion_group, criterion_main, Criterion};

fn bench_init_media_combine(c: &mut Criterion) {
    let init = vec![0u8; 6 * 1024];
    let media = vec![0u8; 2 * 1024 * 1024];

    c.bench_function("init+media memcpy", |b| {
        b.iter(|| {
            let mut combined = Vec::with_capacity(init.len() + media.len());
            combined.extend_from_slice(&init);
            combined.extend_from_slice(&media);
            black_box(combined);
        });
    });

    c.bench_function("init+media zero-copy", |b| {
        b.iter(|| {
            let init = Bytes::from(init.clone());
            let media = Bytes::from(media.clone());
            let combined = init.chain(media);
            black_box(combined);
        });
    });
}

criterion_group!(benches, bench_init_media_combine);
criterion_main!(benches);
```

---

## Rollout Strategy

1. **Phase 1 (Sprint 1):**
   - Implement critical fixes
   - Measure memory/latency improvements
   - Verify no regressions

2. **Phase 2 (Sprint 2):**
   - Implement high priority optimizations
   - Add monitoring metrics
   - Update documentation

3. **Phase 3 (Sprint 3):**
   - Implement medium priority improvements
   - Performance tuning
   - Comprehensive testing

4. **Phase 4 (Optional):**
   - Low priority / future enhancements
   - SIMD optimizations
   - Parallel downloads

---

## Success Criteria

✅ **Must have:**
- Memory usage < 40 MB per stream
- No unbounded memory growth
- Latency reduction > 20%
- All tests passing

✅ **Should have:**
- Allocation rate < 500 KB/s
- Lock contention < 5ms p99
- Binary format for indexes
- Buffer pooling for all layers

✅ **Nice to have:**
- SIMD optimizations
- Parallel segment downloads
- Connection pooling
- Comprehensive benchmarks

---

## Estimated Effort

| Sprint | Tasks | Effort | Impact |
|--------|-------|--------|--------|
| Sprint 1 | 3 tasks | 1-2 days | 30-50 MB savings, critical fixes |
| Sprint 2 | 4 tasks | 2-3 days | 10-20% performance gain |
| Sprint 3 | 4 tasks | 3-5 days | Architecture improvements |
| Sprint 4 | 3 tasks | 5+ days | Nice-to-have features |

**Total critical path:** 6-10 days for high-performance player
**Total with all enhancements:** 11-15 days
