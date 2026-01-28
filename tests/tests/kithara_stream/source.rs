#![cfg(test)]

use std::{
    io::{Read, Seek, SeekFrom},
    time::Duration,
};

use bytes::Bytes;
use kanal::{Receiver, Sender};
use kithara_stream::{BackendCommand, BackendResponse, ChannelReader, RandomAccessBackend};
use rstest::{fixture, rstest};

/// In-memory backend for testing ChannelReader seek.
struct MemBackend {
    cmd_tx: Sender<BackendCommand>,
    data_rx: Receiver<BackendResponse>,
    len: Option<u64>,
}

impl MemBackend {
    fn new(data: Vec<u8>) -> Self {
        let (cmd_tx, cmd_rx) = kanal::bounded(4);
        let (data_tx, data_rx) = kanal::bounded(4);
        let len = Some(data.len() as u64);

        // Spawn worker thread
        std::thread::spawn(move || {
            while let Ok(cmd) = cmd_rx.recv() {
                match cmd {
                    BackendCommand::Read { offset, len } => {
                        let offset = offset as usize;
                        if offset >= data.len() {
                            let _ = data_tx.send(BackendResponse::Eof);
                            continue;
                        }
                        let available = &data[offset..];
                        let n = len.min(available.len());
                        let bytes = Bytes::copy_from_slice(&available[..n]);
                        let _ = data_tx.send(BackendResponse::Data(bytes));
                    }
                    BackendCommand::Seek { .. } => {
                        // No-op for memory backend
                    }
                    BackendCommand::Stop => break,
                }
            }
        });

        Self { cmd_tx, data_rx, len }
    }
}

impl RandomAccessBackend for MemBackend {
    fn data_rx(&self) -> &Receiver<BackendResponse> {
        &self.data_rx
    }

    fn cmd_tx(&self) -> &Sender<BackendCommand> {
        &self.cmd_tx
    }

    fn len(&self) -> Option<u64> {
        self.len
    }
}

/// Backend without known length for testing SeekFrom::End error.
struct UnknownLenBackend {
    cmd_tx: Sender<BackendCommand>,
    data_rx: Receiver<BackendResponse>,
}

impl UnknownLenBackend {
    fn new(data: Vec<u8>) -> Self {
        let (cmd_tx, cmd_rx) = kanal::bounded(4);
        let (data_tx, data_rx) = kanal::bounded(4);

        std::thread::spawn(move || {
            while let Ok(cmd) = cmd_rx.recv() {
                match cmd {
                    BackendCommand::Read { offset, len } => {
                        let offset = offset as usize;
                        if offset >= data.len() {
                            let _ = data_tx.send(BackendResponse::Eof);
                            continue;
                        }
                        let available = &data[offset..];
                        let n = len.min(available.len());
                        let bytes = Bytes::copy_from_slice(&available[..n]);
                        let _ = data_tx.send(BackendResponse::Data(bytes));
                    }
                    BackendCommand::Seek { .. } => {}
                    BackendCommand::Stop => break,
                }
            }
        });

        Self { cmd_tx, data_rx }
    }
}

impl RandomAccessBackend for UnknownLenBackend {
    fn data_rx(&self) -> &Receiver<BackendResponse> {
        &self.data_rx
    }

    fn cmd_tx(&self) -> &Sender<BackendCommand> {
        &self.cmd_tx
    }

    fn len(&self) -> Option<u64> {
        None
    }
}

// ==================== Fixtures ====================

#[fixture]
fn test_data() -> Vec<u8> {
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZ".to_vec()
}

#[fixture]
fn small_data() -> Vec<u8> {
    b"Hello".to_vec()
}

// ==================== SeekFrom::Start tests ====================

#[rstest]
#[case(0, b"ABCDE")]
#[case(5, b"FGHIJ")]
#[case(10, b"KLMNO")]
#[case(20, b"UVWXY")]
#[case(25, b"Z")]
#[timeout(Duration::from_secs(3))]
#[test]
fn seek_start_reads_correct_bytes(
    test_data: Vec<u8>,
    #[case] seek_pos: u64,
    #[case] expected: &[u8],
) {
    let backend = MemBackend::new(test_data);
    let mut reader = ChannelReader::new(backend);

    let pos = reader.seek(SeekFrom::Start(seek_pos)).unwrap();
    assert_eq!(pos, seek_pos);

    let mut buf = vec![0u8; expected.len()];
    let n = reader.read(&mut buf).unwrap();

    assert_eq!(n, expected.len());
    assert_eq!(&buf[..n], expected);
}

#[rstest]
#[timeout(Duration::from_secs(3))]
#[test]
fn seek_start_zero_reads_from_beginning(test_data: Vec<u8>) {
    let backend = MemBackend::new(test_data);
    let mut reader = ChannelReader::new(backend);

    // Read some bytes first
    let mut buf = [0u8; 10];
    let _ = reader.read(&mut buf).unwrap();

    // Seek back to start
    let pos = reader.seek(SeekFrom::Start(0)).unwrap();
    assert_eq!(pos, 0);

    // Read from beginning
    let mut buf = [0u8; 5];
    let n = reader.read(&mut buf).unwrap();

    assert_eq!(n, 5);
    assert_eq!(&buf[..n], b"ABCDE");
}

// ==================== SeekFrom::Current tests ====================

#[rstest]
#[timeout(Duration::from_secs(3))]
#[test]
fn seek_current_forward(test_data: Vec<u8>) {
    let backend = MemBackend::new(test_data);
    let mut reader = ChannelReader::new(backend);

    // Read 5 bytes (position = 5)
    let mut buf = [0u8; 5];
    reader.read(&mut buf).unwrap();
    assert_eq!(&buf, b"ABCDE");

    // Seek forward 5 bytes (position = 10)
    let pos = reader.seek(SeekFrom::Current(5)).unwrap();
    assert_eq!(pos, 10);

    // Read from position 10
    let mut buf = [0u8; 5];
    let n = reader.read(&mut buf).unwrap();

    assert_eq!(n, 5);
    assert_eq!(&buf[..n], b"KLMNO");
}

#[rstest]
#[test]
fn seek_current_backward(test_data: Vec<u8>) {
    let backend = MemBackend::new(test_data);
    let mut reader = ChannelReader::new(backend);

    // Read 10 bytes (position = 10)
    let mut buf = [0u8; 10];
    reader.read(&mut buf).unwrap();

    // Seek backward 5 bytes (position = 5)
    let pos = reader.seek(SeekFrom::Current(-5)).unwrap();
    assert_eq!(pos, 5);

    // Read from position 5
    let mut buf = [0u8; 5];
    let n = reader.read(&mut buf).unwrap();

    assert_eq!(n, 5);
    assert_eq!(&buf[..n], b"FGHIJ");
}

#[rstest]
#[timeout(Duration::from_secs(3))]
#[test]
fn seek_current_zero_stays_at_position(test_data: Vec<u8>) {
    let backend = MemBackend::new(test_data);
    let mut reader = ChannelReader::new(backend);

    // Read 10 bytes
    let mut buf = [0u8; 10];
    reader.read(&mut buf).unwrap();

    // Seek 0 should stay at current position
    let pos = reader.seek(SeekFrom::Current(0)).unwrap();
    assert_eq!(pos, 10);
}

// ==================== SeekFrom::End tests ====================

#[rstest]
#[case(-5, b"VWXYZ")]
#[case(-10, b"QRSTU")]
#[case(-26, b"ABCDE")]
#[timeout(Duration::from_secs(3))]
#[test]
fn seek_end_reads_correct_bytes(
    test_data: Vec<u8>,
    #[case] offset: i64,
    #[case] expected: &[u8],
) {
    let data_len = test_data.len();
    let backend = MemBackend::new(test_data);
    let mut reader = ChannelReader::new(backend);

    let expected_pos = (data_len as i64 + offset) as u64;

    let pos = reader.seek(SeekFrom::End(offset)).unwrap();
    assert_eq!(pos, expected_pos);

    let mut buf = vec![0u8; expected.len()];
    let n = reader.read(&mut buf).unwrap();

    assert_eq!(n, expected.len());
    assert_eq!(&buf[..n], expected);
}

#[rstest]
#[timeout(Duration::from_secs(3))]
#[test]
fn seek_end_zero_seeks_to_eof(test_data: Vec<u8>) {
    let data_len = test_data.len() as u64;
    let backend = MemBackend::new(test_data);
    let mut reader = ChannelReader::new(backend);

    let pos = reader.seek(SeekFrom::End(0)).unwrap();
    assert_eq!(pos, data_len);

    // Read should return 0 (EOF)
    let mut buf = [0u8; 5];
    let n = reader.read(&mut buf).unwrap();
    assert_eq!(n, 0);
}

#[rstest]
#[timeout(Duration::from_secs(3))]
#[test]
fn seek_end_fails_without_known_length(test_data: Vec<u8>) {
    let backend = UnknownLenBackend::new(test_data);
    let mut reader = ChannelReader::new(backend);

    let result = reader.seek(SeekFrom::End(-5));

    assert!(result.is_err());
    let err = result.unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::Unsupported);
}

// ==================== Error cases ====================

#[rstest]
#[timeout(Duration::from_secs(3))]
#[test]
fn seek_past_eof_fails(test_data: Vec<u8>) {
    let data_len = test_data.len() as u64;
    let backend = MemBackend::new(test_data);
    let mut reader = ChannelReader::new(backend);

    let result = reader.seek(SeekFrom::Start(data_len + 10));

    assert!(result.is_err());
    let err = result.unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
}

#[rstest]
#[timeout(Duration::from_secs(3))]
#[test]
fn seek_negative_position_fails(test_data: Vec<u8>) {
    let backend = MemBackend::new(test_data);
    let mut reader = ChannelReader::new(backend);

    let result = reader.seek(SeekFrom::Current(-100));

    assert!(result.is_err());
    let err = result.unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
}

#[rstest]
#[timeout(Duration::from_secs(3))]
#[test]
fn seek_end_positive_offset_past_eof_fails(test_data: Vec<u8>) {
    let backend = MemBackend::new(test_data);
    let mut reader = ChannelReader::new(backend);

    let result = reader.seek(SeekFrom::End(10));

    assert!(result.is_err());
    let err = result.unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
}

// ==================== Multiple seeks ====================

#[rstest]
#[timeout(Duration::from_secs(3))]
#[test]
fn multiple_seeks_work_correctly(test_data: Vec<u8>) {
    let backend = MemBackend::new(test_data);
    let mut reader = ChannelReader::new(backend);
    let mut results = Vec::new();

    // Seek to position 10
    reader.seek(SeekFrom::Start(10)).unwrap();
    let mut buf = [0u8; 1];
    reader.read(&mut buf).unwrap();
    results.push(buf[0]);

    // Seek back to 5
    reader.seek(SeekFrom::Start(5)).unwrap();
    reader.read(&mut buf).unwrap();
    results.push(buf[0]);

    // Seek forward from current (+10)
    reader.seek(SeekFrom::Current(10)).unwrap();
    reader.read(&mut buf).unwrap();
    results.push(buf[0]);

    // Seek from end (-3)
    reader.seek(SeekFrom::End(-3)).unwrap();
    reader.read(&mut buf).unwrap();
    results.push(buf[0]);

    assert_eq!(results[0], b'K');
    assert_eq!(results[1], b'F');
    assert_eq!(results[2], b'Q');
    assert_eq!(results[3], b'X');
}

#[rstest]
#[timeout(Duration::from_secs(3))]
#[test]
fn position_tracks_correctly(test_data: Vec<u8>) {
    let backend = MemBackend::new(test_data);
    let mut reader = ChannelReader::new(backend);
    let mut positions = Vec::new();

    positions.push(reader.position());

    // Read 5 bytes
    let mut buf = [0u8; 5];
    reader.read(&mut buf).unwrap();
    positions.push(reader.position());

    // Seek to 15
    reader.seek(SeekFrom::Start(15)).unwrap();
    positions.push(reader.position());

    // Read 3 more bytes
    let mut buf = [0u8; 3];
    reader.read(&mut buf).unwrap();
    positions.push(reader.position());

    assert_eq!(positions[0], 0);
    assert_eq!(positions[1], 5);
    assert_eq!(positions[2], 15);
    assert_eq!(positions[3], 18);
}

// ==================== Edge cases ====================

#[rstest]
#[timeout(Duration::from_secs(3))]
#[test]
fn seek_and_read_empty_buffer(test_data: Vec<u8>) {
    let backend = MemBackend::new(test_data);
    let mut reader = ChannelReader::new(backend);

    reader.seek(SeekFrom::Start(10)).unwrap();

    // Read with empty buffer
    let mut buf = [];
    let n = reader.read(&mut buf).unwrap();

    // Position should not change
    let pos = reader.position();

    assert_eq!(n, 0);
    assert_eq!(pos, 10);
}

#[rstest]
#[timeout(Duration::from_secs(3))]
#[test]
fn seek_exact_to_last_byte(small_data: Vec<u8>) {
    let len = small_data.len() as u64;
    let backend = MemBackend::new(small_data);
    let mut reader = ChannelReader::new(backend);

    // Seek to last byte
    let pos = reader.seek(SeekFrom::Start(len - 1)).unwrap();
    assert_eq!(pos, len - 1);

    let mut buf = [0u8; 1];
    let n = reader.read(&mut buf).unwrap();

    assert_eq!(n, 1);
    assert_eq!(buf[0], b'o'); // "Hello" -> last byte is 'o'
}

#[rstest]
#[timeout(Duration::from_secs(3))]
#[test]
fn seek_to_exact_eof_returns_zero_on_read(small_data: Vec<u8>) {
    let len = small_data.len() as u64;
    let backend = MemBackend::new(small_data);
    let mut reader = ChannelReader::new(backend);

    // Seek to exactly EOF
    let pos = reader.seek(SeekFrom::Start(len)).unwrap();
    assert_eq!(pos, len);

    let mut buf = [0u8; 10];
    let n = reader.read(&mut buf).unwrap();

    assert_eq!(n, 0);
}
