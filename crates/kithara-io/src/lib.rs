#![forbid(unsafe_code)]

use std::io::{Read, Seek};

use bytes::Bytes;
use kanal::{Receiver, Sender};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum IoError {
    #[error("not implemented")]
    Unimplemented,
    #[error("communication channel closed")]
    ChannelClosed,
    #[error("buffer is full")]
    BufferFull,
}

pub type IoResult<T> = Result<T, IoError>;

#[derive(Clone, Debug)]
pub struct BridgeOptions {
    pub max_buffer_bytes: usize,
}

impl Default for BridgeOptions {
    fn default() -> Self {
        Self {
            max_buffer_bytes: 1024 * 1024,
        }
    }
}

#[derive(Clone, Debug)]
pub struct BridgeWriter {
    tx: Sender<BridgeMsg>,
    max_buffer_bytes: usize,
    current_buffer_bytes: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl BridgeWriter {
    pub fn push(&self, bytes: Bytes) -> IoResult<()> {
        let bytes_len = bytes.len();

        // Check if adding this would exceed buffer limit
        let current = self
            .current_buffer_bytes
            .load(std::sync::atomic::Ordering::Acquire);
        if current + bytes_len > self.max_buffer_bytes {
            return Err(IoError::BufferFull);
        }

        // Reserve space
        self.current_buffer_bytes
            .fetch_add(bytes_len, std::sync::atomic::Ordering::AcqRel);

        // Send message with size tracking
        let msg = BridgeMsg::DataWithSize(bytes, bytes_len);
        self.tx.send(msg).map_err(|_| {
            // If send fails, release the reserved space
            self.current_buffer_bytes
                .fetch_sub(bytes_len, std::sync::atomic::Ordering::AcqRel);
            IoError::ChannelClosed
        })
    }

    pub fn finish(&self) -> IoResult<()> {
        self.tx
            .send(BridgeMsg::EndOfStream)
            .map_err(|_| IoError::ChannelClosed)
    }
}

#[derive(Debug)]
pub struct BridgeReader {
    rx: Receiver<BridgeMsg>,
    current_data: Option<Bytes>,
    read_pos: usize,
    eos_received: bool,
    current_data_size: usize,
    buffer_bytes_used: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl BridgeReader {
    fn new(
        rx: Receiver<BridgeMsg>,
        buffer_bytes_used: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    ) -> Self {
        Self {
            rx,
            current_data: None,
            read_pos: 0,
            eos_received: false,
            current_data_size: 0,
            buffer_bytes_used,
        }
    }
}

impl Read for BridgeReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        // If we have current data, try to read from it
        if let Some(ref data) = self.current_data {
            let remaining = data.len() - self.read_pos;
            if remaining > 0 {
                let to_copy = std::cmp::min(remaining, buf.len());
                buf[..to_copy].copy_from_slice(&data[self.read_pos..self.read_pos + to_copy]);
                self.read_pos += to_copy;

                if self.read_pos == data.len() {
                    // Release bytes from buffer tracking
                    self.buffer_bytes_used
                        .fetch_sub(self.current_data_size, std::sync::atomic::Ordering::AcqRel);
                    self.current_data = None;
                    self.read_pos = 0;
                    self.current_data_size = 0;
                }

                return Ok(to_copy);
            }
        }

        // If we've received EOS and no more data, return 0 (EOF)
        if self.eos_received {
            return Ok(0);
        }

        // Block waiting for next message
        match self.rx.recv() {
            Ok(BridgeMsg::DataWithSize(bytes, size)) => {
                self.current_data = Some(bytes);
                self.current_data_size = size;
                self.read_pos = 0;
                // Recurse to read from the new data
                self.read(buf)
            }
            Ok(BridgeMsg::EndOfStream) => {
                self.eos_received = true;
                // No more data, return EOF
                Ok(0)
            }
            Err(_) => {
                // Channel closed - treat as EOF
                self.eos_received = true;
                Ok(0)
            }
        }
    }
}

impl Seek for BridgeReader {
    fn seek(&mut self, _pos: std::io::SeekFrom) -> std::io::Result<u64> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "seek is not supported in kithara-io bridge",
        ))
    }
}

#[derive(Debug)]
enum BridgeMsg {
    DataWithSize(Bytes, usize),
    EndOfStream,
}

pub fn new_bridge(opts: BridgeOptions) -> (BridgeWriter, BridgeReader) {
    let (tx, rx) = kanal::bounded(1000); // Large enough for messages, we handle byte limiting ourselves
    let buffer_bytes_used = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let writer = BridgeWriter {
        tx,
        max_buffer_bytes: opts.max_buffer_bytes,
        current_buffer_bytes: buffer_bytes_used.clone(),
    };
    let reader = BridgeReader::new(rx, buffer_bytes_used);
    (writer, reader)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, SeekFrom};
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
