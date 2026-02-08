//! Quick check: what does AudioToolbox report as encoder delay (PrimeInfo)
//! for our HLS variants?
//!
//! Usage:
//!   cargo run -p kithara-decode --features apple --example prime_info_check -- /tmp/hls_analysis

use std::{env, fs, io::Cursor, path::Path};

use kithara_decode::{
    Aac, Apple, AppleConfig, AudioDecoder, CodecType, DecoderInput, Flac, PcmSpec,
};
use kithara_stream::ContainerFormat;

fn check_prime_info<C: CodecType>(label: &str, data: Vec<u8>) {
    let cursor = Cursor::new(data);
    let source: Box<dyn DecoderInput> = Box::new(cursor);

    let config = AppleConfig {
        container: Some(ContainerFormat::Fmp4),
        ..Default::default()
    };

    match Apple::<C>::create(source, config) {
        Ok(mut decoder) => {
            let spec: PcmSpec = decoder.spec();

            // Check before decoding
            let before = decoder.prime_info();
            print!("  {label}:");
            print!(
                "  before_decode={:?}",
                before.map(|(l, t)| format!("leading={l} trailing={t}"))
            );

            // Decode a few chunks
            let mut total_frames = 0u64;
            for _ in 0..10 {
                match decoder.next_chunk() {
                    Ok(Some(chunk)) => {
                        total_frames += chunk.frames() as u64;
                    }
                    _ => break,
                }
            }

            // Check after decoding
            let after = decoder.prime_info();
            println!(
                "  after_decode({total_frames}fr)={:?}  [{}Hz {}ch]",
                after.map(|(l, t)| format!("leading={l} trailing={t}")),
                spec.sample_rate,
                spec.channels,
            );
        }
        Err(e) => {
            eprintln!("  {label}: decoder creation failed: {e}");
        }
    }
}

fn main() {
    let base_dir = env::args()
        .nth(1)
        .unwrap_or_else(|| "/tmp/hls_analysis".to_string());
    let base = Path::new(&base_dir);

    println!("AudioToolbox kAudioConverterPrimeInfo check");
    println!("{}", "=".repeat(70));

    let aac_variants = ["slq", "smq", "shq"];
    let flac_variants = ["slossless"];

    // Use multiple segments to give converter more context
    let seg_count = 3;

    for variant in &aac_variants {
        let init = base.join(variant).join("init.mp4");
        let mut data = fs::read(&init).expect("read init");
        for seg in 1..=seg_count {
            let seg_path = base.join(variant).join(format!("seg_{seg}.m4s"));
            data.extend_from_slice(&fs::read(&seg_path).expect("read seg"));
        }
        check_prime_info::<Aac>(&format!("{variant} (AAC)"), data);
    }

    for variant in &flac_variants {
        let init = base.join(variant).join("init.mp4");
        let mut data = fs::read(&init).expect("read init");
        for seg in 1..=seg_count {
            let seg_path = base.join(variant).join(format!("seg_{seg}.m4s"));
            data.extend_from_slice(&fs::read(&seg_path).expect("read seg"));
        }
        check_prime_info::<Flac>(&format!("{variant} (FLAC)"), data);
    }

    println!();
    println!("For comparison, silence detection measured:");
    println!("  slq:       2089 frames (47.4ms)");
    println!("  smq:       2080 frames (47.2ms)");
    println!("  shq:       2096 frames (47.5ms)");
    println!("  slossless: 1062 frames (24.1ms)");
}
