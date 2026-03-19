# Round 2: Cross-Review

## Consensus (All 3 agree)
1. **Epochs/generations must stay** — removing seek_epoch is the most dangerous change
2. **ContentProvider conflates two access patterns** — segments (HLS) vs streams (File)
3. **Sync blocking inside actor is wrong** — must spawn async jobs, stay responsive to Seek/Cancel
4. **Vec<u8> violates pool discipline** — use SharedPool/PooledBuffer
5. **Backpressure can deadlock seek** — separate control and data channels needed
6. **Message payloads too weak** — need variant, duration, epoch, not just bytes+index

## Disagreements
- Codex: "make provider async" isn't enough — collapsed abstraction and lost commit semantics are the real problem
- Codex: partly disagrees with "make actor loop async" — sync control + async sidecar is better fit
- Gemini: disagrees that single container() per provider is weak — provider represents selected source

## Most Dangerous Issue (unanimous)
**Stale data after seek** — without epochs on every message, old in-flight data wins races.
Data flows bottom-up (Downloader→Decoder→Output), seeks flow top-down (Output→Decoder→Downloader).
Without epochs, old pre-seek data arrives after seek reset and gets decoded/played.

## Proposed Solutions

### Codex: Sum type for content access
```rust
enum PipelineInput {
    Segmented(Box<dyn SegmentProvider>),  // HLS: FetchManager + AssetStore
    Linear(Box<dyn LinearSourceFactory>), // File: Read+Seek, FormatReader
}
```
- SegmentProvider wraps FetchManager, keeps commit semantics
- Linear keeps current FileSource/Stream<T>/FormatReader path
- resolve_seek returns segment boundaries only, decoder computes frame skip

### Codex: Sync control + async sidecar pattern
- Actor handles Seek/Cancel/Stop synchronously via recv_sync
- Spawns async fetch jobs with CancellationToken
- Results come back as internal actor messages
- Split control and data channels

### Gemini: SourceStream trait with poll_next_chunk
```rust
trait SourceStream: Send + 'static {
    fn resolve_seek(&mut self, time: Duration) -> Result<StreamAnchor, SourceError>;
    fn poll_next_chunk(&mut self, epoch: u64) -> Poll<Result<StreamChunk, SourceError>>;
}
```

### Gemini: Envelope<M> wrapper for epoch enforcement
```rust
struct Envelope<M> { epoch: u64, payload: M }
```
- Every inter-actor message wrapped in Envelope
- Actor drops messages with epoch < current_epoch
- Seek increments epoch

### Both: Non-blocking fetch pattern
- Spawn async fetch task
- Actor stays responsive to Seek even during in-flight fetch
- Stale fetch results dropped via epoch check on completion
