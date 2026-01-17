#![forbid(unsafe_code)]

//! Tests for SourceReader - sync Read+Seek adapter over async Source.

use std::{
    io::{Read, Seek, SeekFrom},
    ops::Range,
    sync::Arc,
};

use async_trait::async_trait;
use kithara_decode::SourceReader;
use kithara_stream::{MediaInfo, Source, StreamError, StreamResult, WaitOutcome};

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
    type Error = MemorySourceError;

    async fn wait_range(&self, range: Range<u64>) -> StreamResult<WaitOutcome, Self::Error> {
        if range.start >= self.data.len() as u64 {
            Ok(WaitOutcome::Eof)
        } else {
            Ok(WaitOutcome::Ready)
        }
    }

    async fn read_at(&self, offset: u64, buf: &mut [u8]) -> StreamResult<usize, Self::Error> {
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

// ==================== SourceReader Tests ====================

#[tokio::test]
async fn source_reader_read_sequential() {
    let source = Arc::new(MemorySource::from_str("Hello, World!"));
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
}

#[tokio::test]
async fn source_reader_read_all() {
    let source = Arc::new(MemorySource::from_str("Test data"));
    let mut reader = SourceReader::new(source);

    let mut buf = Vec::new();
    let n = reader.read_to_end(&mut buf).unwrap();
    assert_eq!(n, 9);
    assert_eq!(&buf, b"Test data");
}

#[tokio::test]
async fn source_reader_read_eof() {
    let source = Arc::new(MemorySource::from_str("Short"));
    let mut reader = SourceReader::new(source);

    // Read all data
    let mut buf = [0u8; 100];
    let n = reader.read(&mut buf).unwrap();
    assert_eq!(n, 5);

    // Next read should return 0 (EOF)
    let n = reader.read(&mut buf).unwrap();
    assert_eq!(n, 0);
}

#[tokio::test]
async fn source_reader_seek_start() {
    let source = Arc::new(MemorySource::from_str("0123456789"));
    let mut reader = SourceReader::new(source);

    // Read some data
    let mut buf = [0u8; 3];
    reader.read(&mut buf).unwrap();
    assert_eq!(&buf, b"012");

    // Seek to position 5
    let pos = reader.seek(SeekFrom::Start(5)).unwrap();
    assert_eq!(pos, 5);

    // Read from new position
    reader.read(&mut buf).unwrap();
    assert_eq!(&buf, b"567");
}

#[tokio::test]
async fn source_reader_seek_current() {
    let source = Arc::new(MemorySource::from_str("0123456789"));
    let mut reader = SourceReader::new(source);

    // Read to position 3
    let mut buf = [0u8; 3];
    reader.read(&mut buf).unwrap();
    assert_eq!(reader.position(), 3);

    // Seek forward 2 from current
    let pos = reader.seek(SeekFrom::Current(2)).unwrap();
    assert_eq!(pos, 5);

    // Read from new position
    reader.read(&mut buf).unwrap();
    assert_eq!(&buf, b"567");
}

#[tokio::test]
async fn source_reader_seek_current_negative() {
    let source = Arc::new(MemorySource::from_str("0123456789"));
    let mut reader = SourceReader::new(source);

    // Read to position 5
    let mut buf = [0u8; 5];
    reader.read(&mut buf).unwrap();
    assert_eq!(reader.position(), 5);

    // Seek back 3 from current
    let pos = reader.seek(SeekFrom::Current(-3)).unwrap();
    assert_eq!(pos, 2);

    // Read from new position
    reader.read(&mut buf).unwrap();
    assert_eq!(&buf, b"23456");
}

#[tokio::test]
async fn source_reader_seek_end() {
    let source = Arc::new(MemorySource::from_str("0123456789"));
    let mut reader = SourceReader::new(source);

    // Seek to 3 bytes before end
    let pos = reader.seek(SeekFrom::End(-3)).unwrap();
    assert_eq!(pos, 7);

    // Read remaining
    let mut buf = [0u8; 3];
    let n = reader.read(&mut buf).unwrap();
    assert_eq!(n, 3);
    assert_eq!(&buf, b"789");
}

#[tokio::test]
async fn source_reader_seek_to_zero() {
    let source = Arc::new(MemorySource::from_str("Hello"));
    let mut reader = SourceReader::new(source);

    // Read all
    let mut buf = [0u8; 5];
    reader.read(&mut buf).unwrap();
    assert_eq!(reader.position(), 5);

    // Seek back to start
    let pos = reader.seek(SeekFrom::Start(0)).unwrap();
    assert_eq!(pos, 0);

    // Read again
    reader.read(&mut buf).unwrap();
    assert_eq!(&buf, b"Hello");
}

#[tokio::test]
async fn source_reader_seek_past_eof_fails() {
    let source = Arc::new(MemorySource::from_str("Short"));
    let mut reader = SourceReader::new(source);

    // Seek past EOF should fail
    let result = reader.seek(SeekFrom::Start(100));
    assert!(result.is_err());
}

#[tokio::test]
async fn source_reader_seek_negative_fails() {
    let source = Arc::new(MemorySource::from_str("Test"));
    let mut reader = SourceReader::new(source);

    // Seek to negative position should fail
    let result = reader.seek(SeekFrom::Current(-10));
    assert!(result.is_err());
}

#[tokio::test]
async fn source_reader_position_tracking() {
    let source = Arc::new(MemorySource::from_str("0123456789"));
    let mut reader = SourceReader::new(source);

    assert_eq!(reader.position(), 0);

    let mut buf = [0u8; 3];
    reader.read(&mut buf).unwrap();
    assert_eq!(reader.position(), 3);

    reader.seek(SeekFrom::Start(7)).unwrap();
    assert_eq!(reader.position(), 7);

    reader.read(&mut buf).unwrap();
    assert_eq!(reader.position(), 10);
}

#[tokio::test]
async fn source_reader_empty_read() {
    let source = Arc::new(MemorySource::from_str("Test"));
    let mut reader = SourceReader::new(source);

    // Empty buffer read returns 0
    let mut buf = [];
    let n = reader.read(&mut buf).unwrap();
    assert_eq!(n, 0);
    assert_eq!(reader.position(), 0);
}

#[tokio::test]
async fn source_reader_empty_source() {
    let source = Arc::new(MemorySource::new(vec![]));
    let mut reader = SourceReader::new(source);

    let mut buf = [0u8; 10];
    let n = reader.read(&mut buf).unwrap();
    assert_eq!(n, 0);
}

#[tokio::test]
async fn source_reader_large_data() {
    // Test with larger data to verify chunked reading
    let data: Vec<u8> = (0..10000).map(|i| (i % 256) as u8).collect();
    let source = Arc::new(MemorySource::new(data.clone()));
    let mut reader = SourceReader::new(source);

    let mut result = Vec::new();
    reader.read_to_end(&mut result).unwrap();

    assert_eq!(result, data);
}

#[tokio::test]
async fn source_reader_multiple_seeks_and_reads() {
    let source = Arc::new(MemorySource::from_str("ABCDEFGHIJ"));
    let mut reader = SourceReader::new(source);

    let mut buf = [0u8; 2];

    // Read at various positions
    reader.seek(SeekFrom::Start(0)).unwrap();
    reader.read(&mut buf).unwrap();
    assert_eq!(&buf, b"AB");

    reader.seek(SeekFrom::Start(4)).unwrap();
    reader.read(&mut buf).unwrap();
    assert_eq!(&buf, b"EF");

    reader.seek(SeekFrom::Start(8)).unwrap();
    reader.read(&mut buf).unwrap();
    assert_eq!(&buf, b"IJ");

    reader.seek(SeekFrom::Start(2)).unwrap();
    reader.read(&mut buf).unwrap();
    assert_eq!(&buf, b"CD");
}
