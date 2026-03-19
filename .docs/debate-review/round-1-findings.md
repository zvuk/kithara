# Round 1: Initial Findings

## Codex Findings

### ContentProvider
- **Critical**: `fetch_segment() -> Vec<u8>` conflicts with File needing `Read+Seek` for FormatReader. File = one segment breaks for large files and non-fMP4.
- **High**: `resolve_seek()` returns `frame_offset` but playlist only resolves to segment boundaries. Provider shouldn't compute frame skip — that's decoder's job.
- **High**: `num_segments()` / `num_variants()` are infallible sync, but FetchManager's Loader is async+fallible. Need fully-built PlaylistState at construction.
- **Medium**: Single `container()` per provider is weaker than current per-variant `variant_codec()` / `variant_container()`.
- **Medium**: `fetch_init()`/`fetch_segment()` drop metadata — current SegmentMeta carries duration, sequence, url, len, container. Need rich `SegmentReady` struct.

### Async/Sync Bridge
- **Critical**: Blocking sync `fetch_segment()` prevents Cancel processing. Current Backend uses select! between demand/cancel and async work. Downloader must stay async or spawn cancellable jobs.
- **High**: Runtime ownership unspecified. Current Backend already creates dedicated runtime. HlsContentProvider must own dedicated runtime.
- **High**: Current bridge is "async fetch → commit to AssetStore → sync read from cache", not "async fetch → bytes". Losing commit boundary loses DRM semantics.
- **Medium**: CancellationToken must propagate to in-flight fetches. Every fetch needs own token tied to seek.
- **Medium**: Unbounded mailbox + full Vec<u8> segments = memory risk. Need bounded data channel or credit-based delivery.

### Seek Cascade
- **Critical**: `DecoderMsg::Seek` has no target time or epoch in message types, but seek flow uses `Seek { time: 90s }`. Inconsistency.
- **Critical**: Design removes generation/epoch tagging. Current code uses seek_epoch everywhere. FSM analysis doc kept epoch on all messages. Must restore.
- **High**: Seek resolution ownership split — provider has resolve_seek() but decoder flow says decoder computes "90s → segment #14".
- **High**: `DecoderMsg::Segment { data, index }` too weak — no variant, duration, epoch. Current HlsFetch has all of these.
- **High**: SeekComplete emitted on buffer refill without generation check — stale PCM could satisfy it.
- **High**: Backpressure deadlock — if decoder blocked sending old PCM, can't process new Seek. Need separate control/data channels.
- **Medium**: "stay in Ready if init cached" too optimistic — init can be evicted.

### Migration
- **Critical**: "Delete everything first" has no rollback. FSM analysis doc suggested feature-flagged dual path.
- **High**: File playback broken — "File = one segment" but Phase 6 admits MP3/FLAC/WAV need FormatReader.
- **High**: Symphonia fork as hard prereq. Should be behind feature flag.
- **High**: Design doc and branch already out of sync.
- **High**: TDD plan doesn't preserve: demand preemption, init eviction reset, DRM size reconciliation, watermark preservation, tail-gap rewind.
- **Medium**: "kithara-play not affected" is wrong — audio worker tightly coupled to Timeline/seek_pending.

## Gemini Findings

### ContentProvider
- **High**: `Vec<u8>` violates project's "No buffer allocations — use SharedPool from kithara-bufpool". Need pooled buffers.
- **Medium**: No range support. Works for HLS but inefficient for large files.
- **Medium**: Should be async-ready signature even if initially sync.

### Async/Sync Bridge
- **Medium**: Thread starvation — block_on prevents processing Stop/Flush during hung download.
- **Medium**: Missing explicit backpressure between Downloader and FetchManager.
- **Medium**: WASM needs explicit AsyncContentProvider.

### Seek Cascade
- **High**: ABR/Seek race — variant switch + seek in close proximity can cause wrong init probe.
- **High**: Epoch must be strictly in ALL messages since Timeline shared state is removed.
- **Medium**: Sequential seek cascade increases time-to-first-sample. Speculative init fetch could help.

### Migration
- **Medium**: No parallel implementation — can't A/B test.
- **Medium**: Multi-day window of 0 passing tests.
- **Medium**: Side-by-side migration with feature flag recommended.

## Claude Analysis (Round 1)

### ContentProvider
- **Critical**: The trait conflates two fundamentally different access patterns:
  1. Segment-based (HLS): discrete chunks, init+media, multiple variants
  2. Stream-based (File): continuous byte stream, Read+Seek for FormatReader
  Forcing File into segment API creates an impedance mismatch. A `Cursor<Vec<u8>>` over a 500MB FLAC file is not viable.
- **High**: No prefetch/look-ahead API. Current Backend has sophisticated look-ahead with backpressure. ContentProvider is request-response only — no way to tell it "prepare segments 15-20 while I decode 14".
- **Medium**: Error type `ContentError` undefined. Should map to existing HlsError/FileError or be a new unified type.

### Async/Sync Bridge
- **Critical**: The fundamental tension: kithara-actor is sync (recv_sync loop), but all I/O is async (tokio). Three options, design picks worst one (sync wrapper over async). Better: make actor loop async with recv_async(), or use dedicated async task that feeds actor via channel.
- **High**: block_on() inside actor thread creates nested runtime risk if actor is spawned from within tokio context. Backend already learned this lesson (creates dedicated single-thread runtime).

### Seek Cascade
- **High**: The cascade Output→Decoder→Downloader is top-down but data flows bottom-up (Downloader→Decoder→Output). During seek, old data is still in-flight bottom-up while new seek commands go top-down. Without epochs, old data from pre-seek fetch arrives after seek and gets decoded/played.
- **High**: No timeout or deadlock detection in cascade. If downloader fetch hangs, seek never completes. Current code has hang_watchdog! — actor pipeline has none.

### Migration
- **High**: The stashed code (Phases 3-4) already diverged from design and was thrown away. This validates the concern about plan instability. Design should be frozen before more implementation.
