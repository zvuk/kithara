//! Test MockDecoder with Cursor (static data, no HLS).
//!
//! Verifies that MockDecoder can read all segments from a Cursor.

use std::io::Cursor;

mod mock_decoder;
use mock_decoder::MockDecoder;

use kithara_decode::{Decoder, PcmSpec};

/// Helper: create binary segment data
fn make_segment(variant: u8, segment: u32, data_len: usize) -> Vec<u8> {
    let mut data = Vec::new();
    data.push(variant);
    data.extend(&segment.to_be_bytes());
    data.extend(&(data_len as u32).to_be_bytes());
    data.extend(vec![b'A'; data_len]);
    data
}

#[test]
fn test_mock_decoder_reads_all_segments_from_cursor() {
    const TOTAL_SEGMENTS: usize = 10;

    // Create 10 binary segments
    let mut data = Vec::new();
    for segment_idx in 0..TOTAL_SEGMENTS {
        data.extend(make_segment(0, segment_idx as u32, 1000));
    }

    println!("Total data size: {} bytes", data.len());

    // Create Cursor
    let cursor = Cursor::new(data);

    // Create MockDecoder
    let spec = PcmSpec {
        sample_rate: 44100,
        channels: 2,
    };
    let mut decoder = MockDecoder::with_spec(cursor, spec);

    // Read all chunks
    let mut chunks_read = 0;
    loop {
        match decoder.next_chunk() {
            Ok(Some(chunk)) => {
                // Verify chunk metadata
                assert_eq!(chunk.pcm[0], 0.0, "Chunk {} has wrong variant", chunks_read);
                assert_eq!(
                    chunk.pcm[1], chunks_read as f32,
                    "Chunk {} has wrong segment index",
                    chunks_read
                );

                chunks_read += 1;
            }
            Ok(None) => {
                println!("EOF after {} chunks", chunks_read);
                break;
            }
            Err(e) => {
                panic!("Decode error at chunk {}: {}", chunks_read, e);
            }
        }
    }

    assert_eq!(
        chunks_read, TOTAL_SEGMENTS,
        "Should read all {} segments, but read only {}",
        TOTAL_SEGMENTS, chunks_read
    );

    println!("✅ MockDecoder correctly read all {} segments from Cursor!", TOTAL_SEGMENTS);
}
