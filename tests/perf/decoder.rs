//! Performance tests for audio decoder.
//!
//! Run with: `cargo test --test decoder --features perf --release`

#![cfg(feature = "perf")]

use std::io::Cursor;

use kithara::decode::{
    AudioCodec, AudioDecoder, CodecType, ContainerFormat, Symphonia, SymphoniaConfig,
};
use kithara_test_utils::create_test_wav;

/// PCM codec marker for WAV files.
struct Pcm;
impl CodecType for Pcm {
    const CODEC: AudioCodec = AudioCodec::Pcm;
}

type WavDecoder = Symphonia<Pcm>;

fn wav_config() -> SymphoniaConfig {
    SymphoniaConfig {
        container: Some(ContainerFormat::Wav),
        ..Default::default()
    }
}

#[hotpath::measure]
fn decoder_next_chunk_measured(decoder: &mut WavDecoder) -> Option<()> {
    decoder.next_chunk().ok().flatten().map(|_| ())
}

#[test]
#[ignore]
fn perf_decoder_wav_decode_loop() {
    let _guard = hotpath::FunctionsGuardBuilder::new("decoder_wav").build();
    // Create 1 second of audio (44100 samples)
    let wav_data = create_test_wav(44100, 44100, 2);
    let cursor = Cursor::new(wav_data);

    let mut decoder = WavDecoder::create(Box::new(cursor), wav_config()).unwrap();

    let mut chunk_count = 0;

    while decoder_next_chunk_measured(&mut decoder).is_some() {
        chunk_count += 1;
    }

    println!("\n{:=<60}", "");
    println!("WAV Decoder Performance");
    println!("Chunks decoded: {}", chunk_count);
    println!("{:=<60}\n", "");
}

#[hotpath::measure]
fn decoder_probe_single(wav_data: &[u8]) {
    let cursor = Cursor::new(wav_data.to_vec());
    let _decoder = WavDecoder::create(Box::new(cursor), wav_config()).unwrap();
}

#[test]
#[ignore]
fn perf_decoder_probe_latency() {
    let _guard = hotpath::FunctionsGuardBuilder::new("decoder_probe").build();
    let wav_data = create_test_wav(44100, 44100, 2);

    // Measure probe latency (cold start)
    for _ in 0..10 {
        decoder_probe_single(&wav_data);
    }

    println!("\n{:=<60}", "");
    println!("Decoder Probe Latency");
    println!("Iterations: 10");
    println!("{:=<60}\n", "");
}

#[hotpath::measure]
fn decoder_chunk_process(decoder: &mut WavDecoder) -> Option<usize> {
    decoder.next_chunk().ok().flatten().map(|chunk| {
        // F32 conversion happens inside next_chunk
        chunk.pcm.len()
    })
}

#[test]
#[ignore]
fn perf_decoder_f32_conversion() {
    let _guard = hotpath::FunctionsGuardBuilder::new("decoder_f32_conversion").build();
    let wav_data = create_test_wav(44100, 44100, 2);
    let cursor = Cursor::new(wav_data);
    let mut decoder = WavDecoder::create(Box::new(cursor), wav_config()).unwrap();

    let mut chunk_count = 0;
    while decoder_chunk_process(&mut decoder).is_some() {
        chunk_count += 1;
    }

    println!("\n{:=<60}", "");
    println!("F32 Conversion Performance");
    println!("Chunks processed: {}", chunk_count);
    println!("{:=<60}\n", "");
}

/// Measure decoder throughput: how many samples per second can be decoded.
#[test]
#[ignore]
fn perf_decoder_throughput() {
    let _guard = hotpath::FunctionsGuardBuilder::new("decoder_throughput").build();

    // Create 5 seconds of audio
    let wav_data = create_test_wav(44100 * 5, 44100, 2);
    let cursor = Cursor::new(wav_data);
    let mut decoder = WavDecoder::create(Box::new(cursor), wav_config()).unwrap();

    let start = std::time::Instant::now();
    let mut total_samples = 0;

    hotpath::measure_block!("decode_all_chunks", {
        while let Ok(Some(chunk)) = decoder.next_chunk() {
            total_samples += chunk.pcm.len();
        }
    });

    let elapsed = start.elapsed();
    let samples_per_sec = total_samples as f64 / elapsed.as_secs_f64();
    let realtime_factor = samples_per_sec / (44100.0 * 2.0); // stereo

    println!("\n{:=<60}", "");
    println!("Decoder Throughput");
    println!("Total samples: {}", total_samples);
    println!("Elapsed: {:.2?}", elapsed);
    println!("Samples/sec: {:.0}", samples_per_sec);
    println!("Realtime factor: {:.2}x", realtime_factor);
    println!("{:=<60}\n", "");
}
