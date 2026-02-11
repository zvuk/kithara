#![forbid(unsafe_code)]

//! Tests for `SourceReader` - sync `Read`+`Seek` adapter over sync `Source`.

use std::io::{Read, Seek, SeekFrom};

use kithara_audio::SourceReader;
use rstest::{fixture, rstest};

use crate::common::memory_source::MemorySource;

// Fixtures

#[fixture]
fn hello_source() -> MemorySource {
    MemorySource::from_str("Hello, World!")
}

#[fixture]
fn digits_source() -> MemorySource {
    MemorySource::from_str("0123456789")
}

#[fixture]
fn alpha_source() -> MemorySource {
    MemorySource::from_str("ABCDEFGHIJ")
}

#[fixture]
fn empty_source() -> MemorySource {
    MemorySource::new(vec![])
}

#[fixture]
fn large_source() -> MemorySource {
    let data: Vec<u8> = (0..10000).map(|i| (i % 256) as u8).collect();
    MemorySource::new(data)
}

// SourceReader Tests

#[rstest]
#[test]
fn read_sequential(hello_source: MemorySource) {
    let mut reader = SourceReader::new(hello_source);

    let mut buf = [0u8; 5];
    let n = reader.read(&mut buf).unwrap();
    assert_eq!(n, 5);
    assert_eq!(&buf, b"Hello");

    let n = reader.read(&mut buf).unwrap();
    assert_eq!(n, 5);
    assert_eq!(&buf, b", Wor");

    let n = reader.read(&mut buf).unwrap();
    assert_eq!(n, 3);
    assert_eq!(&buf[..3], b"ld!");
}

#[rstest]
#[test]
fn read_all() {
    let source = MemorySource::from_str("Test data");
    let mut reader = SourceReader::new(source);

    let mut buf = Vec::new();
    let n = reader.read_to_end(&mut buf).unwrap();
    assert_eq!(n, 9);
    assert_eq!(&buf, b"Test data");
}

#[rstest]
#[test]
fn read_eof() {
    let source = MemorySource::from_str("Short");
    let mut reader = SourceReader::new(source);

    let mut buf = [0u8; 100];
    let n = reader.read(&mut buf).unwrap();
    assert_eq!(n, 5);

    // Next read should return 0 (EOF)
    let n = reader.read(&mut buf).unwrap();
    assert_eq!(n, 0);
}

#[rstest]
#[case::start(SeekFrom::Start(5), 5, "567")]
#[case::current_forward(SeekFrom::Current(2), 5, "567")]
#[case::end(SeekFrom::End(-3), 7, "789")]
#[test]
fn seek_operations(
    digits_source: MemorySource,
    #[case] seek_from: SeekFrom,
    #[case] expected_pos: u64,
    #[case] expected_data: &str,
) {
    let mut reader = SourceReader::new(digits_source);
    let expected = expected_data.as_bytes().to_vec();

    // Read initial 3 bytes to set position
    let mut buf = [0u8; 3];
    let _n = reader.read(&mut buf).unwrap();

    // Perform seek
    let pos = reader.seek(seek_from).unwrap();
    assert_eq!(pos, expected_pos);

    // Read and verify
    let _n = reader.read(&mut buf).unwrap();
    assert_eq!(&buf[..], &expected[..]);
}

#[rstest]
#[test]
fn seek_current_negative(digits_source: MemorySource) {
    let mut reader = SourceReader::new(digits_source);

    let mut buf = [0u8; 5];
    let _n = reader.read(&mut buf).unwrap();
    assert_eq!(reader.position(), 5);

    let pos = reader.seek(SeekFrom::Current(-3)).unwrap();
    assert_eq!(pos, 2);

    let _n = reader.read(&mut buf).unwrap();
    assert_eq!(&buf, b"23456");
}

#[rstest]
#[test]
fn seek_to_zero() {
    let source = MemorySource::from_str("Hello");
    let mut reader = SourceReader::new(source);

    let mut buf = [0u8; 5];
    let _n = reader.read(&mut buf).unwrap();
    assert_eq!(reader.position(), 5);

    let pos = reader.seek(SeekFrom::Start(0)).unwrap();
    assert_eq!(pos, 0);

    let _n = reader.read(&mut buf).unwrap();
    assert_eq!(&buf, b"Hello");
}

#[rstest]
#[case::past_eof(SeekFrom::Start(100))]
#[case::negative(SeekFrom::Current(-10))]
#[test]
fn seek_invalid_fails(#[case] seek_from: SeekFrom) {
    let source = MemorySource::from_str("Short");
    let mut reader = SourceReader::new(source);
    let result = reader.seek(seek_from);
    assert!(result.is_err());
}

#[rstest]
#[test]
fn position_tracking(digits_source: MemorySource) {
    let mut reader = SourceReader::new(digits_source);

    assert_eq!(reader.position(), 0);

    let mut buf = [0u8; 3];
    let _n = reader.read(&mut buf).unwrap();
    assert_eq!(reader.position(), 3);

    reader.seek(SeekFrom::Start(7)).unwrap();
    assert_eq!(reader.position(), 7);

    let _n = reader.read(&mut buf).unwrap();
    assert_eq!(reader.position(), 10);
}

#[rstest]
#[test]
fn empty_read() {
    let source = MemorySource::from_str("Test");
    let mut reader = SourceReader::new(source);

    let mut buf = [];
    let n = reader.read(&mut buf).unwrap();
    assert_eq!(n, 0);
    assert_eq!(reader.position(), 0);
}

#[rstest]
#[test]
fn empty_source_read(empty_source: MemorySource) {
    let mut reader = SourceReader::new(empty_source);

    let mut buf = [0u8; 10];
    let n = reader.read(&mut buf).unwrap();
    assert_eq!(n, 0);
}

#[rstest]
#[test]
fn large_data_read(large_source: MemorySource) {
    let expected: Vec<u8> = (0..10000).map(|i| (i % 256) as u8).collect();
    let mut reader = SourceReader::new(large_source);

    let mut result = Vec::new();
    reader.read_to_end(&mut result).unwrap();

    assert_eq!(result, expected);
}

#[rstest]
#[test]
fn multiple_seeks_and_reads(alpha_source: MemorySource) {
    let mut reader = SourceReader::new(alpha_source);
    let mut buf = [0u8; 2];

    let test_cases = [(0u64, b"AB"), (4, b"EF"), (8, b"IJ"), (2, b"CD")];

    for (pos, expected) in test_cases {
        reader.seek(SeekFrom::Start(pos)).unwrap();
        let _n = reader.read(&mut buf).unwrap();
        assert_eq!(&buf, expected);
    }
}
