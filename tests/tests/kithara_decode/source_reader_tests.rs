#![forbid(unsafe_code)]

//! Tests for SourceReader - sync Read+Seek adapter over async Source.

use std::{
    io::{Read, Seek, SeekFrom},
    ops::Range,
};

use async_trait::async_trait;
use kithara_decode::SourceReader;
use kithara_stream::{MediaInfo, Source, StreamResult, WaitOutcome};
use rstest::{fixture, rstest};

// ==================== Mock Source ====================

/// In-memory source for testing SourceReader.
struct MemorySource {
    data: Vec<u8>,
}

impl MemorySource {
    fn new(data: Vec<u8>) -> Self {
        Self { data }
    }

    fn from_str(s: &str) -> Self {
        Self::new(s.as_bytes().to_vec())
    }
}

#[derive(Debug, thiserror::Error)]
#[error("MemorySourceError")]
struct MemorySourceError;

#[async_trait]
impl Source for MemorySource {
    type Item = u8;
    type Error = MemorySourceError;

    async fn wait_range(&mut self, range: Range<u64>) -> StreamResult<WaitOutcome, Self::Error> {
        if range.start >= self.data.len() as u64 {
            Ok(WaitOutcome::Eof)
        } else {
            Ok(WaitOutcome::Ready)
        }
    }

    async fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> StreamResult<usize, Self::Error> {
        let offset = offset as usize;
        if offset >= self.data.len() {
            return Ok(0);
        }
        let available = self.data.len() - offset;
        let n = buf.len().min(available);
        buf[..n].copy_from_slice(&self.data[offset..offset + n]);
        Ok(n)
    }

    fn len(&self) -> Option<u64> {
        Some(self.data.len() as u64)
    }

    fn media_info(&self) -> Option<MediaInfo> {
        None
    }
}

// ==================== Fixtures ====================

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

// ==================== SourceReader Tests ====================

#[rstest]
#[tokio::test]
async fn read_sequential(hello_source: MemorySource) {
    let source = hello_source;

    let result = tokio::task::spawn_blocking(move || {
        let mut reader = SourceReader::new(source);

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
    })
    .await;

    result.unwrap();
}

#[rstest]
#[tokio::test]
async fn read_all() {
    let source = MemorySource::from_str("Test data");

    let result = tokio::task::spawn_blocking(move || {
        let mut reader = SourceReader::new(source);

        let mut buf = Vec::new();
        let n = reader.read_to_end(&mut buf).unwrap();
        assert_eq!(n, 9);
        assert_eq!(&buf, b"Test data");
    })
    .await;

    result.unwrap();
}

#[rstest]
#[tokio::test]
async fn read_eof() {
    let source = MemorySource::from_str("Short");

    let result = tokio::task::spawn_blocking(move || {
        let mut reader = SourceReader::new(source);

        let mut buf = [0u8; 100];
        let n = reader.read(&mut buf).unwrap();
        assert_eq!(n, 5);

        // Next read should return 0 (EOF)
        let n = reader.read(&mut buf).unwrap();
        assert_eq!(n, 0);
    })
    .await;

    result.unwrap();
}

#[rstest]
#[case::start(SeekFrom::Start(5), 5, "567")]
#[case::current_forward(SeekFrom::Current(2), 5, "567")]
#[case::end(SeekFrom::End(-3), 7, "789")]
#[tokio::test]
async fn seek_operations(
    digits_source: MemorySource,
    #[case] seek_from: SeekFrom,
    #[case] expected_pos: u64,
    #[case] expected_data: &str,
) {
    let source = digits_source;
    let expected = expected_data.as_bytes().to_vec();

    let result = tokio::task::spawn_blocking(move || {
        let mut reader = SourceReader::new(source);

        // Read initial 3 bytes to set position
        let mut buf = [0u8; 3];
        reader.read(&mut buf).unwrap();

        // Perform seek
        let pos = reader.seek(seek_from).unwrap();
        assert_eq!(pos, expected_pos);

        // Read and verify
        reader.read(&mut buf).unwrap();
        assert_eq!(&buf[..], &expected[..]);
    })
    .await;

    result.unwrap();
}

#[rstest]
#[tokio::test]
async fn seek_current_negative(digits_source: MemorySource) {
    let source = digits_source;

    let result = tokio::task::spawn_blocking(move || {
        let mut reader = SourceReader::new(source);

        let mut buf = [0u8; 5];
        reader.read(&mut buf).unwrap();
        assert_eq!(reader.position(), 5);

        let pos = reader.seek(SeekFrom::Current(-3)).unwrap();
        assert_eq!(pos, 2);

        reader.read(&mut buf).unwrap();
        assert_eq!(&buf, b"23456");
    })
    .await;

    result.unwrap();
}

#[rstest]
#[tokio::test]
async fn seek_to_zero() {
    let source = MemorySource::from_str("Hello");

    let result = tokio::task::spawn_blocking(move || {
        let mut reader = SourceReader::new(source);

        let mut buf = [0u8; 5];
        reader.read(&mut buf).unwrap();
        assert_eq!(reader.position(), 5);

        let pos = reader.seek(SeekFrom::Start(0)).unwrap();
        assert_eq!(pos, 0);

        reader.read(&mut buf).unwrap();
        assert_eq!(&buf, b"Hello");
    })
    .await;

    result.unwrap();
}

#[rstest]
#[case::past_eof(SeekFrom::Start(100))]
#[case::negative(SeekFrom::Current(-10))]
#[tokio::test]
async fn seek_invalid_fails(#[case] seek_from: SeekFrom) {
    let source = MemorySource::from_str("Short");

    let result = tokio::task::spawn_blocking(move || {
        let mut reader = SourceReader::new(source);
        let result = reader.seek(seek_from);
        assert!(result.is_err());
    })
    .await;

    result.unwrap();
}

#[rstest]
#[tokio::test]
async fn position_tracking(digits_source: MemorySource) {
    let source = digits_source;

    let result = tokio::task::spawn_blocking(move || {
        let mut reader = SourceReader::new(source);

        assert_eq!(reader.position(), 0);

        let mut buf = [0u8; 3];
        reader.read(&mut buf).unwrap();
        assert_eq!(reader.position(), 3);

        reader.seek(SeekFrom::Start(7)).unwrap();
        assert_eq!(reader.position(), 7);

        reader.read(&mut buf).unwrap();
        assert_eq!(reader.position(), 10);
    })
    .await;

    result.unwrap();
}

#[rstest]
#[tokio::test]
async fn empty_read() {
    let source = MemorySource::from_str("Test");

    let result = tokio::task::spawn_blocking(move || {
        let mut reader = SourceReader::new(source);

        let mut buf = [];
        let n = reader.read(&mut buf).unwrap();
        assert_eq!(n, 0);
        assert_eq!(reader.position(), 0);
    })
    .await;

    result.unwrap();
}

#[rstest]
#[tokio::test]
async fn empty_source_read(empty_source: MemorySource) {
    let source = empty_source;

    let result = tokio::task::spawn_blocking(move || {
        let mut reader = SourceReader::new(source);

        let mut buf = [0u8; 10];
        let n = reader.read(&mut buf).unwrap();
        assert_eq!(n, 0);
    })
    .await;

    result.unwrap();
}

#[rstest]
#[tokio::test]
async fn large_data_read(large_source: MemorySource) {
    let expected: Vec<u8> = (0..10000).map(|i| (i % 256) as u8).collect();
    let source = large_source;

    let result = tokio::task::spawn_blocking(move || {
        let mut reader = SourceReader::new(source);

        let mut result = Vec::new();
        reader.read_to_end(&mut result).unwrap();

        assert_eq!(result, expected);
    })
    .await;

    result.unwrap();
}

#[rstest]
#[tokio::test]
async fn multiple_seeks_and_reads(alpha_source: MemorySource) {
    let source = alpha_source;

    let result = tokio::task::spawn_blocking(move || {
        let mut reader = SourceReader::new(source);
        let mut buf = [0u8; 2];

        let test_cases = [(0u64, b"AB"), (4, b"EF"), (8, b"IJ"), (2, b"CD")];

        for (pos, expected) in test_cases {
            reader.seek(SeekFrom::Start(pos)).unwrap();
            reader.read(&mut buf).unwrap();
            assert_eq!(&buf, expected);
        }
    })
    .await;

    result.unwrap();
}
