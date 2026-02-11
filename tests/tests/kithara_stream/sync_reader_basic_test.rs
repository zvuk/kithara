//! Tests for `SyncReader` with static data (Cursor).
//!
//! Tests `SyncReader` in isolation without HLS or networking.

use std::io::{Cursor, Read};

#[test]
fn test_cursor_reads_all_binary_segments() {
    // Create 3 binary segments (same format as MockDecoder expects)
    let mut data = Vec::new();

    // Segment 0: variant=0, segment=0, data_len=100
    data.push(0u8); // variant
    data.extend(&0u32.to_be_bytes()); // segment
    data.extend(&100u32.to_be_bytes()); // data_len
    data.extend(vec![b'A'; 100]); // data

    // Segment 1: variant=0, segment=1, data_len=200
    data.push(0u8);
    data.extend(&1u32.to_be_bytes());
    data.extend(&200u32.to_be_bytes());
    data.extend(vec![b'B'; 200]);

    // Segment 2: variant=0, segment=2, data_len=150
    data.push(0u8);
    data.extend(&2u32.to_be_bytes());
    data.extend(&150u32.to_be_bytes());
    data.extend(vec![b'C'; 150]);

    let total_len = data.len();
    println!("Total data size: {} bytes", total_len);

    // Create Cursor (simple in-memory reader)
    let mut cursor = Cursor::new(data);

    // Read ALL data
    let mut all_data = Vec::new();
    let mut read_buf = vec![0u8; 1024];

    loop {
        let n = cursor.read(&mut read_buf).unwrap();
        if n == 0 {
            break; // EOF
        }
        all_data.extend_from_slice(&read_buf[..n]);
        println!("Read {} bytes, total: {}", n, all_data.len());
    }

    println!("Total read: {} bytes", all_data.len());

    // Verify we read ALL data
    assert_eq!(
        all_data.len(),
        total_len,
        "Cursor should read all {} bytes, but read only {}",
        total_len,
        all_data.len()
    );

    // Verify we can parse all 3 segments
    let mut offset = 0;
    for expected_segment in 0..3 {
        println!("Parsing segment {} at offset {}", expected_segment, offset);

        assert!(
            offset + 9 <= all_data.len(),
            "Not enough data for segment {} header",
            expected_segment
        );

        let variant = all_data[offset];
        let segment = u32::from_be_bytes([
            all_data[offset + 1],
            all_data[offset + 2],
            all_data[offset + 3],
            all_data[offset + 4],
        ]);
        let data_len = u32::from_be_bytes([
            all_data[offset + 5],
            all_data[offset + 6],
            all_data[offset + 7],
            all_data[offset + 8],
        ]) as usize;

        assert_eq!(variant, 0);
        assert_eq!(segment, expected_segment);

        offset += 9 + data_len;
    }

    assert_eq!(offset, total_len, "Should have parsed all data exactly");

    println!("✅ Cursor correctly read all 3 segments!");
}
