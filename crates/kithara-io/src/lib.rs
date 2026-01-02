//! # Kithara I/O Bridge
//!
//! Bridge between async byte sources and synchronous I/O consumers.
//! Connects async sources (`kithara-file`, `kithara-hls`) to sync consumers (`kithara-decode`)
//! through a bounded buffer with backpressure.
//!
//! ## Core Components
//!
//! - `BridgeWriter`: Async producer interface
//! - `BridgeReader`: Sync consumer implementing `Read` + `Seek`
//! - `BufferTracker`: Centralized buffer management
//!
//! ## EOF Semantics (Normative)
//!
//! **Contract:** `Read::read()` returns `Ok(0)` **only** after:
//! 1. `BridgeWriter::finish()` is called, AND
//! 2. All buffered data has been consumed.
//!
//! No "false EOFs". When data is unavailable, the reader blocks.
//!
//! ## Seek Contract (Normative)
//!
//! **Contract:** All `Seek` operations return `ErrorKind::Unsupported`.
//! This is explicit behavior for streaming sources that lack random access.
//!
//! ## Backpressure
//!
//! Memory is bounded by `max_buffer_bytes`:
//! - `push()` fails with `BufferFull` at limit
//! - Memory released when data consumed
//! - Prevents unbounded memory growth

#![forbid(unsafe_code)]

pub mod bridge;
pub mod errors;
pub mod reader;
pub mod sync;
pub mod writer;

// Re-export public API
pub use bridge::{BridgeMsg, BridgeOptions, new_bridge};
pub use errors::{IoError, IoResult};
pub use reader::BridgeReader;
pub use writer::BridgeWriter;

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Seek, SeekFrom};
    use std::thread;
    use std::time::Duration;

    #[test]
    fn test_basic_bridge_creation() {
        let opts = BridgeOptions::default();
        let (writer, reader) = new_bridge(opts);

        // Test that we can create the bridge without panicking
        // This test will drive the basic implementation
        drop(writer);
        drop(reader);
    }

    #[test]
    fn test_read_blocks_until_data_then_reads() {
        let opts = BridgeOptions::default();
        let (writer, mut reader) = new_bridge(opts);

        // Start a reader thread that will block until data is available
        let reader_thread = thread::spawn(move || {
            let mut buf = [0u8; 10];
            let bytes_read = reader.read(&mut buf).expect("read should succeed");
            assert!(bytes_read > 0, "should read some data");
            assert_eq!(&buf[..bytes_read], b"hello worl");
            bytes_read
        });

        // Give the reader time to start and block
        thread::sleep(Duration::from_millis(10));

        // Push data - this should unblock the reader
        use bytes::Bytes;
        let data = Bytes::from("hello world");
        writer.push(data).expect("push should succeed");

        // Wait for reader to complete
        let bytes_read = reader_thread.join().expect("reader thread should complete");
        assert_eq!(bytes_read, 10); // buffer size
    }

    #[test]
    fn test_read_returns_0_only_after_finish() {
        let opts = BridgeOptions::default();
        let (writer, mut reader) = new_bridge(opts);

        // Push some data first
        use bytes::Bytes;
        writer
            .push(Bytes::from("test"))
            .expect("push should succeed");

        // Read the data
        let mut buf = [0u8; 10];
        let bytes_read = reader.read(&mut buf).expect("read should succeed");
        assert_eq!(bytes_read, 4);
        assert_eq!(&buf[..4], b"test");

        // Before finish, read should block (not return 0)
        // We can't easily test blocking without timeouts, so let's test
        // that after finish, read returns 0

        // Finish the stream
        writer.finish().expect("finish should succeed");

        // Now read should return 0 (EOF)
        let bytes_read = reader.read(&mut buf).expect("read should succeed");
        assert_eq!(bytes_read, 0, "should return 0 after finish");

        // Subsequent reads should also return 0
        let bytes_read = reader.read(&mut buf).expect("read should succeed");
        assert_eq!(bytes_read, 0, "should continue returning 0 after EOF");
    }

    #[test]
    fn test_empty_buffer_read() {
        let opts = BridgeOptions::default();
        let (_, mut reader) = new_bridge(opts);

        let mut buf = [];
        let bytes_read = reader.read(&mut buf).expect("read should succeed");
        assert_eq!(bytes_read, 0, "empty buffer should return 0");
    }

    #[test]
    fn test_multiple_small_reads() {
        let opts = BridgeOptions::default();
        let (writer, mut reader) = new_bridge(opts);

        // Push data larger than one read
        use bytes::Bytes;
        let data = Bytes::from("hello world, this is a longer message");
        writer.push(data).expect("push should succeed");
        writer.finish().expect("finish should succeed");

        // Read in small chunks
        let mut buf1 = [0u8; 5];
        let bytes1 = reader.read(&mut buf1).expect("read should succeed");
        assert_eq!(bytes1, 5);
        assert_eq!(&buf1, b"hello");

        let mut buf2 = [0u8; 6];
        let bytes2 = reader.read(&mut buf2).expect("read should succeed");
        assert_eq!(bytes2, 6);
        assert_eq!(&buf2, b" world");

        // Continue reading until EOF
        let mut total_read = bytes1 + bytes2;
        let mut remaining_buf = [0u8; 100];

        loop {
            let bytes = reader
                .read(&mut remaining_buf)
                .expect("read should succeed");
            if bytes == 0 {
                break;
            }
            total_read += bytes;
        }

        // Should have read all data
        assert_eq!(total_read, 37); // length of the message
    }

    #[test]
    fn test_backpressure_blocks_push_when_full() {
        let opts = BridgeOptions {
            max_buffer_bytes: 20, // Small buffer to trigger backpressure quickly
        };
        let (writer, _reader) = new_bridge(opts);

        // Push data up to the limit
        use bytes::Bytes;
        assert!(
            writer.push(Bytes::from("10 bytes__")).is_ok(),
            "first push should succeed"
        );
        assert!(
            writer.push(Bytes::from("10 bytes__")).is_ok(),
            "second push should succeed"
        );

        // The third push should fail because buffer is full (20 bytes total)
        let result = writer.push(Bytes::from("would overflow"));
        assert!(result.is_err(), "push should fail when buffer is full");
        assert!(
            matches!(result, Err(IoError::BufferFull)),
            "should return BufferFull error"
        );
    }

    #[test]
    fn test_backpressure_relieved_after_reading() {
        let opts = BridgeOptions {
            max_buffer_bytes: 20,
        };
        let (writer, mut reader) = new_bridge(opts);

        // Fill the buffer
        use bytes::Bytes;
        assert!(writer.push(Bytes::from("message1__")).is_ok());
        assert!(writer.push(Bytes::from("message2__")).is_ok());

        // Third push should fail
        assert!(writer.push(Bytes::from("overflow")).is_err());

        // Read some data to free up space
        let mut buf = [0u8; 10];
        let bytes_read = reader.read(&mut buf).expect("read should succeed");
        assert_eq!(bytes_read, 10);

        // Now push should succeed again
        assert!(writer.push(Bytes::from("success!")).is_ok());
    }

    #[test]
    fn test_seek_behavior_contract() {
        let opts = BridgeOptions::default();
        let (_, mut reader) = new_bridge(opts);

        // All seek operations should return Unsupported error
        let result = reader.seek(SeekFrom::Start(10));
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::Unsupported);

        let result = reader.seek(SeekFrom::Current(5));
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::Unsupported);

        let result = reader.seek(SeekFrom::End(-5));
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::Unsupported);
    }

    #[test]
    fn test_seek_explicit_error_message() {
        let opts = BridgeOptions::default();
        let (_, mut reader) = new_bridge(opts);

        let result = reader.seek(SeekFrom::Start(0));
        assert!(result.is_err());
        let error = result.unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::Unsupported);
        assert!(error.to_string().contains("seek is not supported"));
    }
}
