//! Performance tests for audio decoder.
//!
//! Run with: `cargo test --test decoder --features perf --release`

#![cfg(feature = "perf")]

use kithara_bufpool::pcm_pool;
use kithara_decode::Decoder;
use std::io::Cursor;

/// Create a minimal valid WAV file for testing.
fn create_test_wav(sample_count: usize) -> Vec<u8> {
    let channels = 2u16;
    let sample_rate = 44100u32;
    let bytes_per_sample = 2;
    let data_size = (sample_count * channels as usize * bytes_per_sample) as u32;
    let file_size = 36 + data_size;

    let mut wav = Vec::new();

    // RIFF header
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&file_size.to_le_bytes());
    wav.extend_from_slice(b"WAVE");

    // fmt chunk
    wav.extend_from_slice(b"fmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&channels.to_le_bytes());
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    let byte_rate = sample_rate * channels as u32 * bytes_per_sample as u32;
    wav.extend_from_slice(&byte_rate.to_le_bytes());
    let block_align = channels * bytes_per_sample as u16;
    wav.extend_from_slice(&block_align.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes()); // bits per sample

    // data chunk
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_size.to_le_bytes());

    // Generate sine wave samples
    for i in 0..sample_count {
        let sample = ((i as f32 * 0.01).sin() * 32767.0) as i16;
        for _ in 0..channels {
            wav.extend_from_slice(&sample.to_le_bytes());
        }
    }

    wav
}

#[hotpath::measure]
fn decoder_next_chunk_measured(decoder: &mut Decoder) -> Option<()> {
    decoder.next_chunk().ok().flatten().map(|_| ())
}

#[test]
#[ignore]
fn perf_decoder_wav_decode_loop() {
    let _guard = hotpath::GuardBuilder::new("decoder_wav").build();
    // Create 1 second of audio (44100 samples)
    let wav_data = create_test_wav(44100);
    let cursor = Cursor::new(wav_data);

    let mut decoder =
        Decoder::new_with_probe(cursor, Some("wav"), pcm_pool().clone()).unwrap();

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
    let _decoder = Decoder::new_with_probe(cursor, Some("wav"), pcm_pool().clone()).unwrap();
}

#[test]
#[ignore]
fn perf_decoder_probe_latency() {
    let _guard = hotpath::GuardBuilder::new("decoder_probe").build();
    let wav_data = create_test_wav(44100);

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
fn decoder_chunk_process(decoder: &mut Decoder) -> Option<usize> {
    decoder.next_chunk().ok().flatten().map(|chunk| {
        // F32 conversion happens inside next_chunk
        chunk.pcm.len()
    })
}

#[test]
#[ignore]
fn perf_decoder_f32_conversion() {
    let _guard = hotpath::GuardBuilder::new("decoder_f32_conversion").build();
    let wav_data = create_test_wav(44100);
    let cursor = Cursor::new(wav_data);
    let mut decoder =
        Decoder::new_with_probe(cursor, Some("wav"), pcm_pool().clone()).unwrap();

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
    let _guard = hotpath::GuardBuilder::new("decoder_throughput").build();

    // Create 5 seconds of audio
    let wav_data = create_test_wav(44100 * 5);
    let cursor = Cursor::new(wav_data);
    let mut decoder =
        Decoder::new_with_probe(cursor, Some("wav"), pcm_pool().clone()).unwrap();

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
