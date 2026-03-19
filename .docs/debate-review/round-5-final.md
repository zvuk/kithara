# Round 5: Final Synthesis

## Approval Status

| Reviewer | Verdict | Blocking Conditions |
|----------|---------|---------------------|
| **Codex** | APPROVED with 2 blocking conditions | 1) LinearSourceFactory must stay pull-based/wait-aware for remote files 2) EOF and failure must be distinct in controller protocol |
| **Gemini** | APPROVED unconditionally | All 8 consensus points approved |
| **Claude** | APPROVED | Agrees with both blocking conditions from Codex |

## Resolution of Outstanding Issues

### 1. FormatReader Streaming for Remote Files

**Codex**: Keep linear path pull-based. Decoder opens one progressive `ReadSeekSource`; downloader populates storage and sends readiness messages, not full chunks. Reuse current `wait_range`/`contains_range`/on-demand range model. For tail-moov MP4: fill tail range or complete download before seek acknowledged.

**Gemini**: "Readiness-Gated Actor" — before every `next_packet()`, decoder checks `is_range_ready()`. If not, awaits `Notify` signal from storage layer. Once ready, synchronous read is guaranteed fast (mmap/memory).

**Consensus**: Both agree — decoder must NOT call FormatReader unless data is locally cached. The storage layer (AssetStore) signals readiness. No `WouldBlock` hacks, no `spawn_blocking` per packet.

### 2. WASM Async Sidecar Compatibility

**Codex**: Use `kithara_platform::tokio::task::spawn` for sidecar tasks. Keep control channels on `kithara_platform::sync::mpsc`. Validate bounded `tokio::sync::mpsc` on WASM with integration test before depending on it.

**Gemini**: Switch ALL channels to wrap `tokio::sync::mpsc` (even on native). Unify the split-brain API where `recv_async()` exists only on WASM.

**Consensus**: Both agree the actor loop must be async-capable on all platforms. Gemini's unification is the ideal target but Codex's pragmatism (validate first) is wise. Start with existing `kithara_platform::sync::mpsc` for control, `tokio::sync::mpsc` for data, with WASM integration test as gate.

### 3. Credit Tuning

- Segmented: 2-3 segment prefetch credits (native), 1 on WASM
- Linear: 2MB buffer window
- Decoder→Output: 10 chunks native, 32 chunks WASM (match existing config)
- Expose via `PipelineConfig` struct, platform-specific defaults

### 4. Prefetch Bounds

- Downloader checks `BytePool::available_memory()` before fetch
- If `available < segment_size * 2`, enter Backpressure state
- Wait for BufferReleased event from Decoder/Output before resuming
- Budget: 32-64 MiB native, 8-16 MiB WASM for segmented prefetch

### 5. Error Propagation

- `PipelineError` enum with `fatal: bool` flag
- All actors send errors to Controller via dedicated unbounded event channel
- Controller is sole owner of shutdown/reset
- **EOF must never transport decode failure** (Codex blocking condition)
- Actors may retry transient errors locally before reporting

## Migration Order

### Codex's Order (bottom-up, adapters-first):
1. Epoch + Envelope + PipelineError + controller events
2. Fix EOF-vs-failure distinction in current runtime
3. PipelineInput adapters over existing infrastructure
4. Replace downloader stage (largest shared-state surface)
5. Introduce FrameExtractor
6. Replace output last
7. Delete old atomics

### Gemini's Order (inside-out, decoder-first):
1. Controller + Epoch + Envelope + PipelineConfig ("The Brain")
2. Decoder Actor + FrameExtractor ("The Heart")
3. Downloader Actor + PipelineInput ("The Lungs")
4. Output Actor ("The Mouth")

### Agreed Foundation:
Both agree: **Epoch + Envelope + PipelineError is Phase 1** — everything else depends on it.

## Top Risks

| Risk | Source | Mitigation |
|------|--------|------------|
| Biased select starves data | Codex | Drain control in bounded batches, coalesce stateful commands |
| WASM worker latency | Gemini | Chunk extraction — never decode entire segment in one call |
| Memory exhaustion | Both | Credit-based backpressure + BytePool gating |
| Epoch desync | Both | Every actor validates epoch on every message, no exceptions |
| Stale drops must release byte credits | Codex | Explicit credit release on drop, not just on consume |
| Init-segment ownership | Codex | First-class invariant: actual resource availability, not just metadata |

## Single Most Important Thing

**Codex**: "Epoch ownership across data, control, credits, and failures. If an in-flight unit from epoch N can still consume memory, emit EOF, or acknowledge seek after epoch N+1 starts, the new pipeline will reproduce the same hangs in a cleaner shape."

**Gemini**: "The biased select loop. If Stop or Seek ever gets queued behind a blocked data send, seeks will hang. Control must always preempt data."

**Claude's synthesis**: Both are saying the same thing from different angles. Epoch is the semantic guarantee (no stale data survives). Biased select is the mechanical guarantee (control is never blocked by data). Together they form the core invariant: **seek is always responsive, stale data is always rejected**.
