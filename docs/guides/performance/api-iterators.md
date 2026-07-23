# API, iterators & collections

*Tiers: hot = audio/decode/resampler/stretch/beat/bufpool; warm = stream/hls/file/net/storage/assets/abr/drm/queue/events/play; cold = platform/app/apple/ffi/encode.*

## Ownership & API signatures

**Reference param over a smart-pointer type.** `ptr_arg=deny` keeps this clean.
```rust
// bad
fn open(s: &String, b: &Vec<u8>, p: &PathBuf) { ... }
// good
fn open(s: &str, b: &[u8], p: &Path) { ... }
```
*tier: warm | detector: clippy `ptr_arg` (deny) | already-enforced*

**By-value param the body only reads.**
```rust
// bad
fn score(cfg: DecodeConfig) -> f32 { cfg.gain * 2.0 }
// good
fn score(cfg: &DecodeConfig) -> f32 { cfg.gain * 2.0 }   // take by value only to move/store/consume
```
*tier: warm | detector: clippy `needless_pass_by_value` (warn) | already-enforced*

**`&mut` that never mutates.**
```rust
// bad
fn depth(buf: &mut RingCons) -> usize { buf.len() }
// good
fn depth(buf: &RingCons) -> usize { buf.len() }
```
*tier: warm | detector: opt-in `cargo xtask audit-clippy` (`needless_pass_by_ref_mut`) | preventive*

**Allocate an owned return to satisfy the borrow checker.**
```rust
// bad
fn codec_name(&self) -> String { self.name.to_string() }
// good
fn codec_name(&self) -> &str { &self.name }          // Cow<'_, str> when sometimes-owned
```
Allocate only for genuinely new data; hand back a borrow otherwise.
*tier: warm | detector: manual (overlaps clippy `unnecessary_to_owned`) | preventive*

**`&T` for a small `Copy` type.**
```rust
// bad
fn seek(pos: &u64, idx: &SegmentIdx) { ... }
// good
fn seek(pos: u64, idx: SegmentIdx) { ... }
```
*tier: warm | detector: clippy `trivially_copy_pass_by_ref` (warn) | already-enforced*

## Iterators & collections

**Unfused adapters (`map().flatten()`, `filter().map()`).**
```rust
// bad
segs.iter().map(|s| s.key.parse().ok())        // -> Iterator<Option<T>>
    .filter(Option::is_some).map(Option::unwrap)
segs.iter().map(load_keys).flatten()
// good
segs.iter().filter_map(|s| s.key.parse().ok())
segs.iter().flat_map(load_keys)
```
*tier: warm | detector: clippy `manual_filter_map`/`map_flatten`/`flat_map_option` | already-enforced*

**Clone before you narrow.**
```rust
// bad
frames.iter().cloned().filter(|f| f.voiced).take(8)
// good
frames.iter().filter(|f| f.voiced).take(8).cloned()   // clone only the survivors
```
*tier: hot | detector: opt-in `cargo xtask audit-clippy` (`iter_overeager_cloned`) | preventive*

**`.map(|x| x.clone())` instead of `.cloned()`/`.copied()`.**
Watch for: `iter.map(|x| x.clone())` - `.copied()` for `Copy`, `.cloned()` otherwise.
*tier: hot | detector: ast-grep `perf.prefer-cloned` + clippy `cloned_instead_of_copied` (deny) | already-enforced*

**`iter().count()` where a length exists.**
Watch for: `slice.iter().count()` / `s.chars()...count()` when `.len()` is O(1) - use `slice.len()`.
*tier: warm | detector: ast-grep `perf.no-iter-count` | already-enforced*

**Stable `sort_by` on floats/primitives.** Stability is meaningless under `total_cmp`; the stable sort still pays for an O(n) scratch buffer.
```rust
// bad
onsets.sort_by(f32::total_cmp);
// good
onsets.sort_unstable_by(f32::total_cmp);
```
Live sites: `analysis/beat/grid.rs`, `analysis/beat/analyzer.rs`, `kithara-beat/src/postprocess.rs`.
*tier: hot | detector: proposed ast-grep `perf.no-stable-sort-total-cmp` (M3); today manual | present in kithara*

**Full sort for top-k / median.**
Watch for: `v.sort(); &v[..k]` - use `v.select_nth_unstable_by(k, cmp)`. Not currently present.
*tier: n/a | detector: manual | preventive*

**`for ... { out.push(x) }` over an iterator.**
```rust
// bad
let mut out = Vec::new();
for s in segs.iter() { out.push(s.idx); }
// good
let out: Vec<_> = segs.iter().map(|s| s.idx).collect();   // or out.extend(segs.iter().map(|s| s.idx))
```
*tier: warm | detector: clippy `needless_for_each` + AGENTS red-flags | already-enforced*

**Manual index loop with bounds checks over one slice.**
Watch for: `for i in 0..slice.len() { slice[i] }` - use `.iter().enumerate()` / `chunks_exact(N)`; already applied to the trimmer in W1.
*tier: hot | detector: clippy `needless_range_loop` (deny) | already-enforced*

## Data-structure choice

**Linear membership scan in a loop (O(n|m))**
```rust
// bad: retain re-scans a Vec for every key (kithara-assets flush/core.rs:155)
let alive: Vec<usize> = live_keys().collect();
self.non_durable.retain(|k| alive.contains(k));
// good: build the set once, membership is O(1)
let alive: HashSet<usize> = live_keys().collect();
self.non_durable.retain(|k| alive.contains(k));
```
Most kithara membership already uses `&HashSet` (assets LRU/pin); the flush retain is the live exception.
*tier: warm | detector: manual (backlog `.audit/rules/audit.linear-scan-in-loop.yml`) | present in kithara*

**contains_key + insert double lookup**
```rust
// bad: hashes and probes k twice
if !map.contains_key(&k) { map.insert(k, make()); }
// good: one lookup, value built only on vacancy
map.entry(k).or_insert_with(make);
```
*tier: warm | detector: clippy `map_entry` (default-on) | already-enforced (no occurrence)*

**HashMap + post-sort instead of BTreeMap**
```rust
// bad: hash map re-sorted every time you need ordered output
let mut out: Vec<_> = map.iter().collect();
out.sort_by_key(|(k, _)| *k);
// good: BTreeMap yields keys in order for free
for (k, v) in &ordered { /* ... */ }
```
Only switch when you *repeatedly* iterate in key order; a one-shot sort of a small Vec (abr candidates) is fine, don't over-rotate to BTreeMap.
*tier: warm | detector: manual | preventive (not present)*

**Vec::remove(0) / insert(0,..) as a FIFO (O(n) shift)**
```rust
// bad: remove(0) shifts every remaining element on each pop
let next = queue.remove(0);
// good: VecDeque, O(1) at both ends
let next = queue.pop_front();   // queue.push_back(item)
```
The hot gapless trimmer queue was moved off `remove(0)` in W1; remaining `remove(0)` lives only in cold sim waker lists (`kithara-platform/src/flash/tokio/mpsc.rs`).
*tier: hot | detector: manual (backlog ast-grep `audit.vec-as-queue`, promotable) | hot instance already fixed*

**ZST map values** - watch for `HashMap<K, ()>` used as a set; use `HashSet<K>`. Absent today (sets are declared `HashSet`); would surface via the opt-in pedantic sweep if introduced. *tier: n/a | detector: clippy `zero_sized_map_values` (pedantic, opt-in) | preventive*
