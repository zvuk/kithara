//! Performance tests for audio decoder.
//!
//! Run with: `cargo test --test decoder --features perf --release`

#![cfg(feature = "perf")]

use std::io::Cursor;

use kithara::decode::{DecoderConfig, DecoderFactory, InnerDecoder};
use kithara_test_utils::create_test_wav;
use rstest::rstest;

fn create_wav_decoder(frames: usize) -> Box<dyn InnerDecoder> {
    let wav_data = create_test_wav(frames, 44100, 2);
    let cursor = Cursor::new(wav_data);
    DecoderFactory::create_with_probe(cursor, Some("wav"), DecoderConfig::default()).unwrap()
}

#[hotpath::measure]
fn decoder_next_chunk_measured(decoder: &mut Box<dyn InnerDecoder>) -> Option<()> {
    decoder.next_chunk().ok().flatten().map(|_| ())
}

#[hotpath::measure]
fn decoder_probe_single(wav_data: &[u8]) {
    let cursor = Cursor::new(wav_data.to_vec());
    let _decoder =
        DecoderFactory::create_with_probe(cursor, Some("wav"), DecoderConfig::default()).unwrap();
}

#[hotpath::measure]
fn decoder_chunk_process(decoder: &mut Box<dyn InnerDecoder>) -> Option<usize> {
    decoder.next_chunk().ok().flatten().map(|chunk| {
        // F32 conversion happens inside next_chunk
        chunk.pcm.len()
    })
}

#[derive(Clone, Copy)]
enum PerfScenario {
    DecodeLoop,
    F32Conversion,
    ProbeLatency,
    Throughput,
}

#[rstest]
#[case("decoder_wav", PerfScenario::DecodeLoop)]
#[case("decoder_probe", PerfScenario::ProbeLatency)]
#[case("decoder_f32_conversion", PerfScenario::F32Conversion)]
#[case("decoder_throughput", PerfScenario::Throughput)]
#[test]
#[ignore]
fn perf_decoder_scenarios(#[case] label: &'static str, #[case] scenario: PerfScenario) {
    let _guard = hotpath::FunctionsGuardBuilder::new(label).build();
    match scenario {
        PerfScenario::DecodeLoop => {
            let mut decoder = create_wav_decoder(44100);
            let mut chunk_count = 0;
            while decoder_next_chunk_measured(&mut decoder).is_some() {
                chunk_count += 1;
            }

            println!("\n{:=<60}", "");
            println!("WAV Decoder Performance");
            println!("Chunks decoded: {}", chunk_count);
            println!("{:=<60}\n", "");
        }
        PerfScenario::ProbeLatency => {
            let wav_data = create_test_wav(44100, 44100, 2);
            for _ in 0..10 {
                decoder_probe_single(&wav_data);
            }

            println!("\n{:=<60}", "");
            println!("Decoder Probe Latency");
            println!("Iterations: 10");
            println!("{:=<60}\n", "");
        }
        PerfScenario::F32Conversion => {
            let mut decoder = create_wav_decoder(44100);
            let mut chunk_count = 0;
            while decoder_chunk_process(&mut decoder).is_some() {
                chunk_count += 1;
            }

            println!("\n{:=<60}", "");
            println!("F32 Conversion Performance");
            println!("Chunks processed: {}", chunk_count);
            println!("{:=<60}\n", "");
        }
        PerfScenario::Throughput => {
            let mut decoder = create_wav_decoder(44100 * 5);

            let start = std::time::Instant::now();
            let mut total_samples = 0;
            hotpath::measure_block!("decode_all_chunks", {
                while let Ok(Some(chunk)) = decoder.next_chunk() {
                    total_samples += chunk.pcm.len();
                }
            });

            let elapsed = start.elapsed();
            let samples_per_sec = total_samples as f64 / elapsed.as_secs_f64();
            let realtime_factor = samples_per_sec / (44100.0 * 2.0);

            println!("\n{:=<60}", "");
            println!("Decoder Throughput");
            println!("Total samples: {}", total_samples);
            println!("Elapsed: {:.2?}", elapsed);
            println!("Samples/sec: {:.0}", samples_per_sec);
            println!("Realtime factor: {:.2}x", realtime_factor);
            println!("{:=<60}\n", "");
        }
    }
}
