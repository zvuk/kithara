# WASM Examples — Phase 1: ABR Demo

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Create a cross-platform (desktop + WASM) example app that demonstrates
the kithara ABR algorithm running in the browser via WebAssembly.

**Architecture:** New `crates/kithara-wasm` bin crate depends on `kithara-abr`.
`fn main()` runs a simulated ABR scenario with `println!()` output (works on both
native and wasm32 via wasm-server-runner). No wasm-bindgen/web-sys needed for Phase 1.

**Tech Stack:** `kithara-abr`, `wasm-server-runner`, stable Rust.

**Design doc:** `docs/plans/2026-02-15-wasm-examples-design.md`

---

### Task 1: Create kithara-wasm crate skeleton

**Files:**
- Create: `crates/kithara-wasm/Cargo.toml`
- Create: `crates/kithara-wasm/src/main.rs`
- Modify: `Cargo.toml` (root, workspace members + wasm-release profile)

**Step 1: Create Cargo.toml**

```toml
[package]
name = "kithara-wasm"
version.workspace = true
edition.workspace = true
authors.workspace = true
license.workspace = true
description = "WASM examples for kithara audio streaming library"
publish = false

[dependencies]
kithara-abr = { workspace = true }

[lints]
workspace = true
```

**Step 2: Create src/main.rs**

```rust
fn main() {
    println!("kithara-wasm: hello from main");
}
```

**Step 3: Add to workspace members in root Cargo.toml**

Add `"crates/kithara-wasm"` to the `members` array (after `"crates/kithara-bufpool"`).

**Step 4: Add wasm-release profile to root Cargo.toml**

Append after `[workspace.dependencies]`:

```toml
[profile.wasm-release]
inherits = "release"
opt-level = "z"
lto = "fat"
codegen-units = 1
```

**Step 5: Verify native build**

Run: `cargo build -p kithara-wasm`
Expected: compiles without errors.

Run: `cargo run -p kithara-wasm`
Expected: prints `kithara-wasm: hello from main`

**Step 6: Commit**

```bash
git add crates/kithara-wasm/ Cargo.toml Cargo.lock
git commit -m "feat: add kithara-wasm crate skeleton"
```

---

### Task 2: Add WASM build infrastructure

**Files:**
- Create: `.cargo/config.toml`

**Step 1: Ensure wasm32 target is installed**

Run: `rustup target add wasm32-unknown-unknown`

**Step 2: Ensure wasm-server-runner is installed**

Run: `cargo install wasm-server-runner`

**Step 3: Create .cargo/config.toml**

```toml
[target.wasm32-unknown-unknown]
runner = "wasm-server-runner"
```

**Step 4: Verify WASM compilation**

Run: `cargo build -p kithara-wasm --target wasm32-unknown-unknown`
Expected: compiles without errors. If `tracing` or any dependency fails,
investigate and add cfg-gates.

**Step 5: Commit**

```bash
git add .cargo/config.toml
git commit -m "chore: add wasm-server-runner config"
```

---

### Task 3: Implement ABR demo

**Files:**
- Modify: `crates/kithara-wasm/src/main.rs`

The demo simulates a realistic ABR scenario with time-shifted `Instant` values
(no `thread::sleep`, works on both native and wasm32). Shows:
1. Initial state (no throughput data)
2. High bandwidth → up-switch to lossless
3. Bandwidth drops → down-switch to lowest quality
4. Bandwidth recovers → up-switch to medium quality
5. Buffer-aware: low buffer prevents up-switch

**Step 1: Write the full ABR demo**

```rust
use std::time::{Duration, Instant};

use kithara_abr::{
    AbrController, AbrDecision, AbrMode, AbrOptions, AbrReason, ThroughputSample,
    ThroughputSampleSource, Variant,
};

fn main() {
    println!("=== Kithara ABR Demo ===");
    println!("Adaptive bitrate selection for audio streaming\n");

    let variants = vec![
        Variant { variant_index: 0, bandwidth_bps: 66_000 },
        Variant { variant_index: 1, bandwidth_bps: 134_000 },
        Variant { variant_index: 2, bandwidth_bps: 270_000 },
        Variant { variant_index: 3, bandwidth_bps: 1_000_000 },
    ];

    println!("Variants:");
    println!("  [0] AAC  66 kbps  (low quality)");
    println!("  [1] AAC 134 kbps  (medium quality)");
    println!("  [2] AAC 270 kbps  (high quality)");
    println!("  [3] FLAC   1 Mbps (lossless)");

    demo_throughput_switching(&variants);
    demo_buffer_awareness(&variants);

    println!("\n=== Demo complete ===");
}

/// Demonstrate throughput-based variant switching.
fn demo_throughput_switching(variants: &[Variant]) {
    println!("\n--- Demo 1: Throughput-based switching ---");

    let options = AbrOptions {
        variants: variants.to_vec(),
        mode: AbrMode::Auto(Some(0)),
        min_switch_interval: Duration::ZERO,
        min_buffer_for_up_switch_secs: 0.0,
        ..Default::default()
    };

    let mut ctrl = AbrController::new(options);
    let base = Instant::now();

    // Phase 1: No data yet
    println!("\n  [Phase 1] No throughput data:");
    let d = ctrl.decide(base);
    print_decision(&d, &ctrl);

    // Phase 2: High bandwidth (2 Mbps) — should up-switch to lossless
    println!("\n  [Phase 2] High bandwidth (2 Mbps):");
    feed_samples(&mut ctrl, base, 0, 5, 2_000_000);
    let d = ctrl.decide(base + Duration::from_secs(5));
    ctrl.apply(&d, base + Duration::from_secs(5));
    print_decision(&d, &ctrl);

    // Phase 3: Bandwidth drops to 100 kbps — should down-switch
    println!("\n  [Phase 3] Bandwidth drops to 100 kbps:");
    feed_samples(&mut ctrl, base, 6, 15, 100_000);
    let d = ctrl.decide(base + Duration::from_secs(15));
    ctrl.apply(&d, base + Duration::from_secs(15));
    print_decision(&d, &ctrl);

    // Phase 4: Bandwidth recovers to 400 kbps — should up-switch to medium
    println!("\n  [Phase 4] Bandwidth recovers to 400 kbps:");
    feed_samples(&mut ctrl, base, 16, 25, 400_000);
    let d = ctrl.decide(base + Duration::from_secs(25));
    ctrl.apply(&d, base + Duration::from_secs(25));
    print_decision(&d, &ctrl);
}

/// Demonstrate buffer-level awareness in up-switch decisions.
fn demo_buffer_awareness(variants: &[Variant]) {
    println!("\n--- Demo 2: Buffer-aware switching ---");

    let options = AbrOptions {
        variants: variants.to_vec(),
        mode: AbrMode::Auto(Some(0)),
        min_switch_interval: Duration::ZERO,
        min_buffer_for_up_switch_secs: 10.0,
        ..Default::default()
    };

    let mut ctrl = AbrController::new(options);
    let base = Instant::now();

    // Feed high bandwidth but low buffer
    println!("\n  [Phase 1] High bandwidth (2 Mbps), buffer only 3s:");
    feed_samples_with_buffer(&mut ctrl, base, 0, 5, 2_000_000, 0.6);
    let d = ctrl.decide(base + Duration::from_secs(5));
    print_decision(&d, &ctrl);
    println!("    Buffer level: {:.1}s (< 10.0s minimum)", ctrl.buffer_level_secs());

    // Feed more data — buffer fills up
    println!("\n  [Phase 2] Buffer fills to 12s:");
    feed_samples_with_buffer(&mut ctrl, base, 6, 12, 2_000_000, 1.5);
    let d = ctrl.decide(base + Duration::from_secs(12));
    ctrl.apply(&d, base + Duration::from_secs(12));
    print_decision(&d, &ctrl);
    println!("    Buffer level: {:.1}s (>= 10.0s minimum)", ctrl.buffer_level_secs());
}

fn feed_samples(
    ctrl: &mut AbrController<kithara_abr::ThroughputEstimator>,
    base: Instant,
    start_sec: u64,
    end_sec: u64,
    bandwidth_bps: u64,
) {
    feed_samples_with_buffer(ctrl, base, start_sec, end_sec, bandwidth_bps, 0.0);
}

fn feed_samples_with_buffer(
    ctrl: &mut AbrController<kithara_abr::ThroughputEstimator>,
    base: Instant,
    start_sec: u64,
    end_sec: u64,
    bandwidth_bps: u64,
    buffer_per_sample_secs: f64,
) {
    for t in start_sec..end_sec {
        let bytes = (bandwidth_bps / 8).max(16_001); // ensure above MIN_CHUNK_BYTES
        ctrl.push_throughput_sample(ThroughputSample {
            bytes,
            duration: Duration::from_secs(1),
            at: base + Duration::from_secs(t),
            source: ThroughputSampleSource::Network,
            content_duration: if buffer_per_sample_secs > 0.0 {
                Some(Duration::from_secs_f64(buffer_per_sample_secs))
            } else {
                None
            },
        });
    }
    let bps = ctrl.buffer_level_secs();
    println!("    Fed {} samples, buffer: {bps:.1}s", end_sec - start_sec);
}

fn print_decision(
    decision: &AbrDecision,
    ctrl: &AbrController<kithara_abr::ThroughputEstimator>,
) {
    let name = match decision.target_variant_index {
        0 => "AAC 66kbps",
        1 => "AAC 134kbps",
        2 => "AAC 270kbps",
        3 => "FLAC 1Mbps",
        _ => "unknown",
    };
    println!(
        "    -> variant {} ({}) | {:?} | changed: {}",
        decision.target_variant_index,
        name,
        decision.reason,
        decision.changed,
    );
    let _ = ctrl; // suppress unused warning when not printing extra info
}
```

**Step 2: Verify on desktop**

Run: `cargo run -p kithara-wasm`
Expected: prints ABR demo scenario showing variant switching.

**Step 3: Verify WASM compilation**

Run: `cargo build -p kithara-wasm --target wasm32-unknown-unknown`
Expected: compiles without errors.

**Step 4: Commit**

```bash
git add crates/kithara-wasm/src/main.rs
git commit -m "feat(wasm): add ABR demo example"
```

---

### Task 4: Test in browser

**Step 1: Launch wasm-server-runner**

Run: `cargo run -p kithara-wasm --target wasm32-unknown-unknown`
Expected: opens browser at `http://127.0.0.1:1334`.

**Step 2: Verify console output**

Open browser DevTools (F12) → Console tab.
Expected: ABR demo output matching the desktop output.

If `println!()` output doesn't show in console, we may need to add
`console_error_panic_hook` for panic messages. For `println!()`,
wasm-server-runner should redirect it automatically.

**Step 3: If wasm-server-runner doesn't redirect println**

Add `console_error_panic_hook` as a dependency and initialize in main:

```toml
# Cargo.toml
[target.'cfg(target_arch = "wasm32")'.dependencies]
console_error_panic_hook = "0.1"
```

```rust
// At top of main():
#[cfg(target_arch = "wasm32")]
console_error_panic_hook::set_once();
```

**Step 4: Final commit**

```bash
git add -A
git commit -m "feat(wasm): ABR demo runs in browser via wasm-server-runner"
```

---

### Task 5: Add CI verification

**Files:**
- Modify: `.gitlab-ci.yml` or scripts (optional)

**Step 1: Add wasm32 build check to CI**

Add to an appropriate CI script or as a standalone check:

```bash
rustup target add wasm32-unknown-unknown
cargo build -p kithara-wasm --target wasm32-unknown-unknown
```

This ensures WASM compilation doesn't regress.

**Step 2: Commit if changes made**

```bash
git add scripts/ .gitlab-ci.yml
git commit -m "ci: add wasm32 build check"
```

---

## Expected output

```
=== Kithara ABR Demo ===
Adaptive bitrate selection for audio streaming

Variants:
  [0] AAC  66 kbps  (low quality)
  [1] AAC 134 kbps  (medium quality)
  [2] AAC 270 kbps  (high quality)
  [3] FLAC   1 Mbps (lossless)

--- Demo 1: Throughput-based switching ---

  [Phase 1] No throughput data:
    -> variant 0 (AAC 66kbps) | NoEstimate | changed: false

  [Phase 2] High bandwidth (2 Mbps):
    Fed 5 samples, buffer: 0.0s
    -> variant 3 (FLAC 1Mbps) | UpSwitch | changed: true

  [Phase 3] Bandwidth drops to 100 kbps:
    Fed 9 samples, buffer: 0.0s
    -> variant 0 (AAC 66kbps) | DownSwitch | changed: true

  [Phase 4] Bandwidth recovers to 400 kbps:
    Fed 9 samples, buffer: 0.0s
    -> variant 1 (AAC 134kbps) | UpSwitch | changed: true

--- Demo 2: Buffer-aware switching ---

  [Phase 1] High bandwidth (2 Mbps), buffer only 3s:
    Fed 5 samples, buffer: 3.0s
    -> variant 0 (AAC 66kbps) | BufferTooLowForUpSwitch | changed: false
    Buffer level: 3.0s (< 10.0s minimum)

  [Phase 2] Buffer fills to 12s:
    Fed 6 samples, buffer: 12.0s
    -> variant 3 (FLAC 1Mbps) | UpSwitch | changed: true
    Buffer level: 12.0s (>= 10.0s minimum)

=== Demo complete ===
```

## Next phases

After Phase 1 is complete, proceed with:
- **Phase 2:** Symphonia decode demo (see design doc)
- **Phase 3:** Full HLS player with cfg-gates (see design doc)
