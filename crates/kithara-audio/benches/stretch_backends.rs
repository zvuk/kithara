//! CPU comparison of the time-stretch backends. Decision aid, not a
//! pass/fail test: prints a table of realtime factors per backend × speed
//! over a deterministic in-code fixture. Native-only.
//!
//! Benches exactly the backends in `StretchBackendKind::ALL`. By default
//! that is the always-on pure-Rust `Timestretch`:
//!
//! ```text
//! cargo bench -p kithara-audio --bench stretch_backends
//! ```
//!
//! To also bench the native C++ backends, add the features to the
//! kithara-audio dev-dependency self-ref so the bench links a lib that has
//! them compiled in (Cargo pins a bench's link features to that dev-dep
//! declaration, not to `--features`):
//!
//! ```toml
//! # crates/kithara-audio/Cargo.toml [dev-dependencies]
//! kithara-audio = { path = ".", default-features = false,
//!   features = ["mock", "probe", "symphonia", "stretch-signalsmith", "stretch-bungee"] }
//! ```
//!
//! (kept out of the committed dev-dep so the default `cargo test` needs no
//! C++ toolchain), then run with `--features stretch-signalsmith,stretch-bungee`.

use std::sync::Arc;
use std::time::Instant;

use kithara_audio::{AudioEffect, StretchBackendKind, TimeStretchProcessor};
use kithara_bufpool::PcmPool;
use kithara_decode::{PcmChunk, PcmMeta, PcmSpec};
use portable_atomic::AtomicF32;

const SR: u32 = 44_100;
const CH: u16 = 2;
const SECONDS: usize = 5;
const CHUNK_FRAMES: usize = 4096;
const SPEEDS: [f32; 4] = [0.75, 1.0, 1.25, 1.5];

fn f32_of(x: f64) -> f32 {
    num_traits::cast(x).unwrap_or_default()
}

fn f64_of(x: usize) -> f64 {
    num_traits::cast(x).unwrap_or_default()
}

/// Deterministic stereo fixture: three summed sines plus a per-beat click,
/// to exercise both tonal and transient handling.
fn fixture() -> Vec<f32> {
    let frames = SECONDS * usize::try_from(SR).unwrap_or(SR as usize);
    let channels = usize::from(CH);
    let mut out = Vec::with_capacity(frames * channels);
    let tau = std::f64::consts::TAU;
    let (mut p1, mut p2, mut p3) = (0.0_f64, 0.0_f64, 0.0_f64);
    let (i1, i2, i3) = (
        tau * 110.0 / f64::from(SR),
        tau * 440.0 / f64::from(SR),
        tau * 1760.0 / f64::from(SR),
    );
    let click_period = usize::try_from(SR).unwrap_or(44_100) / 2; // 120 BPM
    for n in 0..frames {
        let tonal = 0.3 * p1.sin() + 0.2 * p2.sin() + 0.1 * p3.sin();
        let click = if n % click_period < 64 { 0.4 } else { 0.0 };
        let s = f32_of(tonal + click);
        out.push(s);
        out.push(s);
        p1 += i1;
        p2 += i2;
        p3 += i3;
    }
    out
}

fn chunk(samples: &[f32]) -> PcmChunk {
    let frames = samples.len() / usize::from(CH);
    PcmChunk::new(
        PcmMeta {
            spec: PcmSpec {
                channels: CH,
                sample_rate: SR,
            },
            frames: u32::try_from(frames).unwrap_or(0),
            ..Default::default()
        },
        PcmPool::default().attach(samples.to_vec()),
    )
}

/// Process the whole fixture once and report (`output_frames`, `wall_seconds`).
fn run(kind: StretchBackendKind, speed: f32, fixture: &[f32]) -> (usize, f64) {
    let pool = PcmPool::default().clone();
    let tempo = Arc::new(AtomicF32::new(speed));
    let mut fx = TimeStretchProcessor::new(
        kind,
        PcmSpec {
            channels: CH,
            sample_rate: SR,
        },
        Arc::clone(&tempo),
        pool,
    );
    let block = CHUNK_FRAMES * usize::from(CH);
    // Warm up (allocate internal buffers) before timing.
    if let Some(first) = fixture.get(..block.min(fixture.len())) {
        let _ = fx.process(chunk(first));
    }
    fx.reset();

    let mut out_frames = 0usize;
    let start = Instant::now();
    for data in fixture.chunks(block) {
        if let Some(c) = fx.process(chunk(data)) {
            out_frames += c.frames();
        }
    }
    while let Some(c) = fx.flush() {
        out_frames += c.frames();
    }
    (out_frames, start.elapsed().as_secs_f64())
}

fn main() {
    let fixture = fixture();
    let in_frames = fixture.len() / usize::from(CH);
    let audio_secs = f64_of(in_frames) / f64::from(SR);

    println!("\ntime-stretch backend CPU comparison");
    println!("fixture: {SECONDS}s stereo @ {SR} Hz, {CHUNK_FRAMES}-frame chunks\n");
    println!(
        "{:<14} {:>6} {:>10} {:>11} {:>9} {:>11}",
        "backend", "speed", "frames_in", "frames_out", "wall_ms", "x_realtime"
    );
    for &kind in StretchBackendKind::ALL {
        for &speed in &SPEEDS {
            let (out_frames, wall) = run(kind, speed, &fixture);
            let x_realtime = if wall > 0.0 { audio_secs / wall } else { f64::INFINITY };
            println!(
                "{:<14} {:>6.2} {:>10} {:>11} {:>9.1} {:>10.1}x",
                kind,
                speed,
                in_frames,
                out_frames,
                wall * 1000.0,
                x_realtime
            );
        }
    }
    println!("\nx_realtime > 1.0 = faster than realtime at that setting.");
}
