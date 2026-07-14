# Async & concurrency

*Tiers: hot = audio/decode/resampler/stretch/beat/bufpool; warm = stream/hls/file/net/storage/assets/abr/drm/queue/events/play; cold = platform/app/apple/ffi/encode.*

## Async & concurrency

**Lock guard held across `.await`**
```rust
// bad: parking_lot guard alive at the await point -> blocks tasks, deadlock risk
let g = self.state.lock();
fetch(&g.url).await;
// good: clone out under the sync guard, drop it, then await
let url = { self.state.lock().url.clone() };
fetch(&url).await;
```
*tier: warm | detector: clippy `await_holding_lock` + devtools `await_under_guard` | already-enforced*

**Blocking / heavy CPU in an async context** (same concern as `L1`)
```rust
// bad: decode/CPU or a blocking syscall on a tokio worker stalls the reactor
async fn run(&self) { let pcm = decode_whole_file(&src); }
// good: long-lived decode on an owned RT thread; one-shot blocking via bounded spawn_blocking
let decoder = spawn_blocking(move || Decoder::open(src)).await?;
```
*tier: hot | detector: enforced ast-grep `arch.no-raw-no-block` + rtsan-async lane | already-enforced*

**Lock scope wider than the critical section**
```rust
// bad: guard held through unrelated work
let g = self.map.lock();
let v = g.get(&k).copied();
do_unrelated_work();          // still locked
// good: scope the guard to just the access
let v = { self.map.lock().get(&k).copied() };
do_unrelated_work();
```
*tier: warm | detector: clippy `significant_drop_tightening` (enabled) | preventive*

**Sequential await of independent futures**
```rust
// bad: independent fetches serialized
let key = load_key(u1).await?;
let seg = load_seg(u2).await?;
// good: run concurrently, short-circuit on first error (HLS playlist/key loads)
let (key, seg) = try_join!(load_key(u1), load_seg(u2))?;   // or try_join_all(futs)
```
*tier: warm | detector: ast-grep `idioms.join-of-join-alls` (narrow) / manual | already-enforced*

**Write-lock where a read-lock suffices**
```rust
// bad
let n = state.write().len();
// good
let n = state.read().len();
```
*tier: warm | detector: clippy `readonly_write_lock` (default warn) | preventive*

**`Mutex<bool>`/`Mutex<int>` for a lone flag**
```rust
// bad: control flag behind a mutex
inserted: Mutex<bool>,
// good: atomic when the flag stands alone (not coupled with other guarded state)
inserted: AtomicBool,   // store(Release) / load(Acquire)
```
*tier: cold | detector: opt-in `cargo xtask audit-clippy` (`mutex_atomic`) | present in kithara (kithara-ffi `item.rs`)*

## Async/tokio deep

**Blocking syscalls / heavy CPU on the async worker** (anchor; `E2` folds here)
```rust
// bad: heavy CPU pinned on the shared runtime
tokio::spawn(async move { let pcm = decode_track(src); });
// good: decode/resample on an owned RT thread; bound one-shot blocking in spawn_blocking
let decoder = spawn_blocking(move || build_decoder(src)).await?;
// the produce/decode loop lives on the dedicated core, never a tokio worker
```
*tier: hot | detector: enforced ast-grep `arch.no-raw-no-block` + rtsan-async + poll-budget census | already-enforced*

**Unbounded try_recv drain starving siblings**
```rust
// bad: unbounded drain hogs the worker
while let Ok(x) = rx.try_recv() { heavy(x); }
// good: bound to control-plane volume, keep the body trivial (kithara-hls peer eviction)
while let Ok(key) = self.eviction_rx.try_recv() { self.evict(key); }
```
*tier: warm | detector: no_block poll-budget census (partial) / manual | present in kithara (bounded, low-risk)*

**Oversized future state (large values across `.await`)**
```rust
// bad: owned buffer captured across await bloats the future
async fn push(&self, buf: Vec<f32>) { self.sink.send(buf).await; }
// good: pass a cheap handle; move big buffers into spawn_blocking closures, not futures
async fn push(&self, buf: Bytes) { self.sink.send(buf).await; }
```
Drop large locals before the first await; `Box::pin` only at spawn/recursion boundaries.
*tier: hot | detector: opt-in `cargo xtask audit-clippy` (`large_futures`) | preventive*

**`select!` loop recreating stateful futures each turn**
```rust
// bad: fresh sleep()/future rebuilt every turn loses timer state
loop { select! { _ = sleep(dt) => tick(), m = rx.recv() => on(m) } }
// good: hoist with interval/tokio::pin!, add biased where fairness is wasted
let mut ticks = interval(dt);
loop { select! { biased; _ = ticks.tick() => tick(), m = rx.recv() => on(m) } }
```
Keep only cancel-safe ops in `select!`.
*tier: warm | detector: manual | preventive (discipline already followed: kithara-hls `peer.rs` biased + hoisted arms)*

**`tokio::spawn` per tiny item in a loop**
```rust
// bad: one task per micro-CPU item
for x in items { tokio::spawn(async move { tiny(x) }); }
// good: one long-lived worker fed by bounded mpsc; batch with recv_many
let n = rx.recv_many(&mut buf, 32).await;   // one wakeup drains up to 32
for x in buf.drain(..n) { tiny(x); }
```
One spawn per network fetch stays fine - that is bounded IO concurrency, not per-item CPU.
*tier: warm | detector: ast-grep `arch.no-raw-tokio-task` (routes spawns, doesn't detect per-item) / manual | preventive*

**Unbounded channel without backpressure** (`L11` folds here)
```rust
// bad: unbounded producer, no backpressure
let (tx, rx) = mpsc::unbounded_channel::<ResourceKey>();
// good: bounded to the buffering target -> send().await throttles the producer
let (tx, rx) = mpsc::channel::<ResourceKey>(cap);
// pick by intent: watch=latest-state, mpsc=queue, oneshot=RPC reply, broadcast=fan-out
```
Reserve `unbounded_channel` for provably rate-limited producers and justify it in a comment; at the RT edge use a fixed-capacity SPSC ring.
*tier: warm | detector: manual (backlog `unbounded_channel(` census) | present in kithara (kithara-hls `stream/hls.rs`, kithara-stream `downloader.rs` - control-plane, uncommented)*

**`tokio::sync::Mutex` for plain data** (across-await half is `E1`)
```rust
// bad: async mutex guarding plain data, or a std guard held across await
data: tokio::sync::Mutex<State>,
// good: sync platform mutex, lock-mutate-drop before any await
data: kithara_platform::sync::Mutex<State>,   // async mutex only if the section spans .await
```
*tier: warm | detector: clippy `await_holding_lock` + devtools `await_under_guard` | already-enforced*

**Thin async wrapper (needless state machine)**
```rust
// bad: fresh async state machine wrapping a single await
async fn get(&self, k: K) -> V { self.inner.get(k).await }
// good: forward the callee's future (std::future::ready() for constants)
fn get(&self, k: K) -> impl Future<Output = V> + '_ { self.inner.get(k) }
```
*tier: cold | detector: enforced ast-grep `rust.no-thin-async-wrapper` | already-enforced*

**Watch-for (absent in kithara, no example):**
- **`buffer_unordered`/`FuturesUnordered` with a heavy consumer body** - a fat `for_each` serializes in-flight IO; spawn real tasks (`JoinSet` + permits), keep the body trivial. *tier: warm | detector: none | preventive*
- **`#[instrument]` on per-poll / per-chunk fns** - span setup on every call; instrument at task/segment granularity with `skip_all`, use counters for rates. *tier: warm | detector: none | preventive*
- **`Runtime::new().block_on(...)` per call** - rebuilds the runtime each time; hold one long-lived `Handle` and block only at the true FFI boundary (kithara uses a shared `FFI_RUNTIME`). *tier: cold | detector: none | preventive*
- **`span.enter()` held across `.await`** - corrupts trace nesting; use `.instrument(span)` on the future. *tier: warm | detector: none | preventive*
- **`block_in_place` as a default blocking escape hatch** - prefer bounded `spawn_blocking` or an owned thread with channel handoff; structurally blocked by the tokio-routing rules. *tier: warm | detector: enforced ast-grep `arch.no-raw-tokio-task` | already-enforced*
