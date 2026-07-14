# Allocation & memory

*Tiers: hot = audio/decode/resampler/stretch/beat/bufpool; warm = stream/hls/file/net/storage/assets/abr/drm/queue/events/play; cold = platform/app/apple/ffi/encode.*

## Allocation & memory

**Collect-then-iterate roundtrip**
```rust
// bad
let lens: Vec<_> = segs.iter().map(|s| s.len).collect();
let total: usize = lens.iter().sum();
// good
let total: usize = segs.iter().map(|s| s.len).sum();
```
*tier: warm | detector: `perf.no-collect-iter-roundtrip` (ast-grep) | already-enforced*

**Needless clone / clone-assign**
```rust
// bad
sink.push(item.clone());   // item only borrowed downstream
dst = src.clone();         // in a reuse loop: reallocates every pass
// good
sink.push(&item);          // or move once if truly consumed
dst.clone_from(&src);      // reuses dst's existing allocation
```
*tier: warm | detector: opt-in `cargo xtask audit-clippy` (redundant_clone / assigning_clones) | preventive*

**Grow without reserve**
```rust
// bad
let mut out = Vec::new();
for f in frames { out.push(map(f)); }
// good
let mut out = Vec::with_capacity(frames.len());  // beat analyzer: with_capacity(frames*2)
out.extend(frames.iter().map(map));
```
*tier: warm | detector: manual (red-flags "allocate in loops") | preventive*

**Hot-loop alloc / capacity churn**
```rust
// bad
for block in blocks {
    let mut scratch = vec![0.0; n];  // fresh alloc every block
    dsp(block, &mut scratch);
    scratch.shrink_to_fit();         // hands the pages right back
}
// good
let mut scratch = pcm_pool.get_with(|b| b.resize(n, 0.0));
for block in blocks { scratch.clear(); scratch.resize(n, 0.0); dsp(block, &mut scratch); }
```
`clear()` keeps capacity for the next pass; `shrink` only at lifecycle boundaries (track unload / cache evict) to a watermark, as bufpool's `shrink_to(trim)` does.
*tier: hot | detector: none (manual; overlaps `perf.prefer-*-pool`) | preventive*

**PCM/byte buffers outside kithara-bufpool** (flagship)
```rust
// bad
let mut buf = vec![0.0f32; frames * channels];  // RT/decode path
let raw = pcm_pool();                            // global accessor
// good
let mut buf = self.pcm_pool.get_with(|b| { b.clear(); b.resize(frames * channels, 0.0); });
// PooledOwned recycles on drop; take PcmPool/BytePool from the app top, never the global accessor
```
*tier: hot | detector: `perf.prefer-pcm-pool` / `perf.prefer-byte-pool` / `perf.no-global-pool-accessor` (ast-grep) | already-enforced*

**format! for concat / owned compare**
```rust
// bad
let path = format!("{}{}", dir, name);
if codec.to_string() == "flac" { .. }
// good
let mut path = dir.to_owned(); path.push_str(name);  // or write! / concat
if codec == "flac" { .. }
```
*tier: cold | detector: clippy `useless_format` / `cmp_owned` | already-enforced*

**Needless owning conversion**
```rust
// bad
fn open(url: String, key: Vec<u8>) {..}  // body only reads
// good
fn open(url: &str, key: &[u8]) {..}
```
*tier: warm | detector: clippy `unnecessary_to_owned` (+ `ptr_arg`=deny) | already-enforced*

**Double allocation**
```rust
// bad
cover: Arc<String>,  key: Arc<Vec<u8>>,
// good
cover: Arc<str>,     key: Arc<[u8]>,
```
*tier: warm | detector: clippy `redundant_allocation` / `box_collection` | already-enforced*

**vec! for fixed data**
```rust
// bad
for r in vec![Rate::Hz44100, Rate::Hz48000] { .. }
// good
for r in [Rate::Hz44100, Rate::Hz48000] { .. }
```
*tier: cold | detector: clippy `useless_vec` | already-enforced*

**drain-collect**
```rust
// bad
let taken: Vec<_> = buf.drain(..).collect();
// good
let taken = mem::take(&mut buf);  // whole buffer; else drain(..).extend/for_each keeps the tail
```
*tier: warm | detector: clippy `drain_collect` | already-enforced*

**Large value / fat enum variant**
```rust
// bad
enum Event { Tick, Decoded(PcmChunk /* inline, bloats every Event */) }
fn feed(chunk: PcmChunk) {..}
// good
enum Event { Tick, Decoded(Box<PcmChunk>) }
fn feed(chunk: &PcmChunk) {..}
```
An unboxed fat variant sizes *every* enum value to the largest arm.
*tier: hot | detector: clippy `large_enum_variant` / `needless_pass_by_value` | already-enforced*

**Write-only collection**
```rust
// bad
let mut seen = Vec::new();
for e in events { seen.push(e.id); }  // seen never read
// good
// delete seen and the loop that fills it
```
*tier: warm | detector: opt-in `cargo xtask audit-clippy` (collection_is_never_read) | preventive*

**SmallVec/array micro-swap** - watch for: swap `Vec` for `SmallVec`/array only after a benchmark proves the collection is genuinely usually-small; it is frequently cargo-culted and copies on spill. *tier: n/a | detector: manual | preventive*

## Allocators, arenas, and ownership

**Per-packet buffer allocation in the streaming loop**
```rust
// bad: fresh buffer per packet churns the long-lived heap
loop {
    let mut buf = Vec::with_capacity(SEG);
    fill(&mut buf, packet); decode(&buf);
}
// good: lease from the pool, clear + refill, returns on drop
loop {
    let mut buf = pool.get();      // kithara-bufpool, fixed size class
    buf.clear();
    fill(&mut buf, packet); decode(&buf);
}
```
Segregate lifetimes: recurring buffers on pools (chosen over `thread_local` for cross-task ownership), parse-and-discard scratch on a single-thread `thread_local! RefCell<Vec>` with `clear()`-on-entry; keep churn off long-lived caches. *tier: warm | detector: perf.prefer-byte-pool + perf.prefer-pcm-pool + perf.no-collect-iter-roundtrip (enforced ast-grep) | present in kithara*

**Media payloads copied instead of ref-counted**
```rust
// bad: deep-copies the segment to share/slice it
let payload: Vec<u8> = resp.to_vec();
cache.put(payload.clone());            // full copy
decode(&payload[range].to_vec());      // another copy
// good: refcount bump + O(1) alloc-free view
let payload: Bytes = resp;             // BytesMut::freeze() at the producer
cache.put(payload.clone());            // bump
decode(payload.slice(range));          // zero-copy slice
```
Hand payloads across fetch->cache->decode as `bytes::Bytes`; share immutable IDs/URLs as `Arc<str>`. Stray `to_vec` remains only in cold FFI spots (symphonia demuxer, android CSD). *tier: warm | detector: manual | present in kithara*

**Arc clone / `load_full()` in the RT block loop**
```rust
// bad: atomic refcount bump every audio block
loop { let cfg = shared.load_full(); render(&cfg, block); }
// good: clone once at setup; Guard-load per block; drop off-RT
let cfg = shared.clone();              // one bump, off the hot loop
loop {
    let g = params.load();             // ArcSwap Guard, no refcount bump
    render(&cfg, &g, block);
}                                       // last Arc drop -> trash_tx, off the RT thread
```
Pre-warm the ArcSwap debt node before playback so the first store does not allocate on the RT thread. *tier: hot | detector: manual (blocking subset caught by rtsan / no_block lanes) | present in kithara*

**Arc as ownership glue for single-owner values**
```rust
// bad: per-field Arcs with no actual sharing
struct Deck { track: Arc<Track>, meta: Arc<Meta> }
// good: one owner, borrows down; one coarse Arc only where truly shared
struct Deck { track: Track, meta: Meta }
fn render(deck: &Deck) { /* &-borrow */ }
```
Use snapshot/command models across threads, not per-field Arcs. `Rc<RefCell<Node>>` parent/child webs do not occur here; a real graph should use slotmap index handles or petgraph, and `Rc<Box<_>>` is caught by default clippy. *tier: warm | detector: AGENTS.md red-flag gate + arch.no-arc-mutex-godmap/collection (enforced ast-grep) + clippy redundant_allocation | already-enforced*

**Transient peak pins wasm linear memory**
```rust
// bad: whole-segment collect + worst-case scratch on a wasm path
let pcm: Vec<f32> = decode_all(seg).collect();   // peak is permanent
// good: stream through a pre-warmed pooled buffer
let mut buf = pool.get();                         // pre-warmed at init
for frame in decode(seg) { sink.push(&mut buf, frame); }
```
WASM `memory.grow` is one-way, so every transient peak stays resident. Pre-warm the dlmalloc `thread_local` at init; never `wee_alloc`. *tier: warm | detector: manual | preventive (kithara ships a wasm build)*

**Global-allocator swap without a target benchmark**
```rust
// bad: cargo-culted, unbenchmarked
#[global_allocator] static A: Mimalloc = Mimalloc;
// good: no swap unless a per-target bench (p99 decode-loop + phys_footprint) earns it
```
kithara ships the system allocator; RT lanes stay allocation-free regardless of the choice. Keep any swap per-target in one place, benched against real segment decode. *tier: cold | detector: manual | preventive (no global-allocator swap present)*

**Allocating error / Display path**
```rust
// bad: eager format + join in Display for a maybe-unused error
let e = Error::Msg(format!("bad segment {id}"));
// good: thiserror variant + lazy context on the error branch only
#[derive(thiserror::Error)] enum E { #[error("bad segment {0}")] Seg(u32) }
res.with_context(|| format!("segment {idx}"))?;
```
Model expected conditions as enum variants/`Option`, not boxed strings; kithara standardizes on thiserror (snafu rejected). *tier: warm | detector: rust.no-inherent-to-string + rust.no-to-string-method (enforced ast-grep) + clippy to_string_trait_impl | already-enforced*

**Watch-for (not present in kithara, no example needed)**
- **Boxed node graph instead of arena** - per-node `Box`/`Rc` object soup in a phase-scoped tree: use bumpalo (Copy/POD) or typed-arena (runs Drop), scoped to the phase. No such structure exists here. *tier: warm | detector: manual*
- **`Rc<RefCell<Node>>` / `Arc<RwLock<>>` pointer web as data model** - use slotmap index handles or owner+IDs. Only config-level `Arc<RwLock<..>>` exists, already governed by arch.no-arc-mutex-godmap. *tier: warm | detector: enforced ast-grep*
- **serde owned `String`/`Vec` for borrowable input** - deserialize with `&'de str` / `#[serde(borrow)] Cow<'de, str>` and keep the source `Bytes` alive. kithara uses rkyv + a custom HLS parser, so no serde-borrow path. *tier: warm | detector: manual*
- **mimalloc MADV_FREE inflates Apple RSS** - only if mimalloc is ever adopted on Apple: pin >= 3.1.4 or `MIMALLOC_PURGE_DECOMMITS=1`, and measure phys_footprint, not RSS. No mimalloc dependency today. *tier: cold | detector: manual*
- **Manual zeroing loop defeats zero-pages** - `vec![0.0f32; n]` (lowers to `alloc_zeroed`) for fresh buffers, `buf.fill(0.0)` on the live slice for reused RT buffers; never a hand-rolled zero loop. Existing `push(0)` sites are single-element padding, not zero-fills. *tier: hot | detector: manual*
- **dhat heap-count assertions in the parallel libtest harness** - if a no-alloc RT contract test is adopted, put each `dhat::assert!` in its own integration-test file (own process), since libtest runs tests in parallel. dhat is not currently used. *tier: warm | detector: manual*
