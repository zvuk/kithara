# Strings, logging, IO & FFI

*Tiers: hot = audio/decode/resampler/stretch/beat/bufpool; warm = stream/hls/file/net/storage/assets/abr/drm/queue/events/play; cold = platform/app/apple/ffi/encode.*

## Strings, logging & IO

**Allocating tracing / telemetry on the audio path**
```rust
// bad: `?`/`%` field args evaluate at every callsite even when TRACE is off
trace!(stats = ?compute_stats(&buf));
// good: gate the expensive field, or fold per-frame data into a relaxed counter
if tracing::enabled!(Level::TRACE) { trace!(stats = ?compute_stats(&buf)); }
self.frames.fetch_add(n, Relaxed);                 // UI polls the atomic

// bad: format! string / cloned Vec payload per decoded chunk
bus.publish(Event::Progress(format!("read {read}/{total}")));
// good: typed Copy enum through the bounded, deferred EventBus (drop-oldest on full)
bus.enqueue(ReadProgress { read, total });          // one per chunk, flushed later
```
Also floor out verbosity in ship builds via tracing's `release_max_level_info` (not set in any Cargo.toml today).
*tier: warm | detector: manual | present in kithara (EventBus enqueue/flush; hot loops already field-clean)*

**Premature itoa/ryu for control-rate numbers**
```rust
// bad: pull in itoa/ryu to format a once-per-request Range header
// good: control-rate Display is fine; reserve itoa/ryu for per-frame numeric IO (kithara has none)
impl fmt::Display for RangeSpec { /* ... */ }        // range.to_string() once per HTTP GET
```
*tier: warm | detector: enforced-ast-grep (rust.no-inherent-to-string) | already-enforced*

**Fresh, un-sized buffer per IO read**
```rust
// bad: grow from empty, then hand off
let mut buf = Vec::new();
while let Some(c) = stream.next().await { buf.extend_from_slice(&c); }
Bytes::from(buf)
// good: pre-size from the already-parsed Content-Length; zero-copy handoff
let mut buf = Vec::with_capacity(content_length);
// ...extend...
Bytes::from(buf)                                     // per-segment scratch -> kithara-bufpool
```
*tier: warm | detector: enforced-ast-grep (perf.prefer-byte-pool) | present in kithara (Bytes handoff; missing with_capacity in net/client body_bytes)*

Absent in kithara today - watch for regressions:
- **`BufRead::lines()` per-line String** - parse small manifests from one `str::from_utf8(data)` + borrowed line iteration; `lines()` allocates a String per line. *(kithara-hls parses borrowed.)*
- **`serde_json::from_reader` on a raw File** - use `from_str`/`from_slice` over in-memory bytes; if a file path appears, `fs::read` then `from_slice`. *(All JSON is `from_str` today.)*
- **Unbuffered byte-at-a-time IO** - container/ADTS parsing goes through symphonia's buffered `MediaSourceStream` over in-memory `Bytes`/mmap, never raw `File`/`TcpStream` per-field reads.
- **mmap cargo-cult for sequential reads** - mmap only for random-access index/cache behind the owning `AtomicChunked<MmapDriver>`; stream segments with sized reads/`Bytes`.
- **`println!` re-locking stdout in a loop** - dev-tooling `println!` is sanctioned (`rust.no-debug-prints` bans it in prod); in a chatty loop lock stdout once into a `BufWriter` and `writeln!`.
- **Vectored-IO / concat-before-write** - for header+payload writes prefer `bytes::Buf::chain` or a sized `BufWriter` over `[hdr, body].concat()`; check `is_write_vectored()` before trusting `write_vectored` (std default writes only the first slice).

## FFI & platform boundaries

**Chatty fine-grained FFI / wasm getters**
```rust
// bad: per-frame getter marshals a record/String across the boundary on every call
#[uniffi::export] fn now_playing(&self) -> TrackInfo { /* String+Vec via copying RustBuffer */ }

// good: coarse control-plane - opaque handles, scalar getters, one batched snapshot, push observer
#[uniffi::export] fn current_time(&self) -> f64 { ... }
#[uniffi::export] fn snapshot(&self) -> FfiPlayerSnapshot { ... }               // batched state
#[uniffi::export] fn set_observer(&self, obs: Arc<dyn PlayerObserver>) { ... }  // push, don't poll
// wasm-bindgen: same rule - numeric IDs, batch snapshots, DSP stays in the worker
#[wasm_bindgen] pub fn current_time_ms_js(&self) -> f64 { ... }
```
Why: UniFFI lowers every `String`/`Vec`/record/enum through a copying RustBuffer (UTF-8 per call); wasm-bindgen copies via TextEncoder/TextDecoder per crossing. The RT audio callback stays entirely in Rust behind opaque `Arc<AudioPlayerItem>` handles.
*tier: cold | detector: manual | present in kithara (facade scalars + snapshot + observer)*

Absent in kithara today - watch for regressions:
- **Mobile periodic-polling wakeup storm** - drive buffer-health/position/ABR from events, not independent `tokio::time::interval` polls; coalesce/stop timers when paused or backgrounded; route all timing through kithara-platform. *(Prevented by arch.no-direct-time / no-implicit-clock / no-implicit-sleep.)*

All three sections are in hand. Here is the dense markdown for the assigned sections.
