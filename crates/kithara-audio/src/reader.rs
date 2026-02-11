//! Sync Read+Seek adapter over sync Source.
//!
//! Simple adapter without prefetch.
//! HLS already buffers segments ahead, so data is usually ready.

use std::io::{Read, Seek, SeekFrom};

use kithara_stream::{MediaInfo, Source, StreamError, WaitOutcome};

/// Sync reader over sync byte Source.
///
/// Calls Source methods directly (no async, no `block_on`).
pub struct SourceReader<S: Source> {
    source: S,
    pos: u64,
}

impl<S: Source> SourceReader<S> {
    /// Create a new reader.
    pub fn new(source: S) -> Self {
        Self { source, pos: 0 }
    }

    /// Get current media info from source.
    ///
    /// This reflects the media info of data currently being read.
    pub fn media_info(&self) -> Option<MediaInfo> {
        self.source.media_info()
    }

    /// Get current position.
    pub fn position(&self) -> u64 {
        self.pos
    }
}

impl<S: Source> Read for SourceReader<S> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        let range = self.pos..self.pos.saturating_add(buf.len() as u64);

        // Wait for data to be available
        match self.source.wait_range(range) {
            Ok(WaitOutcome::Ready) => {}
            Ok(WaitOutcome::Eof) => return Ok(0),
            Err(e) => {
                return Err(std::io::Error::other(e.to_string()));
            }
        }

        // Read data
        match self.source.read_at(self.pos, buf) {
            Ok(n) => {
                self.pos = self.pos.saturating_add(n as u64);
                Ok(n)
            }
            Err(e) => Err(std::io::Error::other(e.to_string())),
        }
    }
}

impl<S: Source> Seek for SourceReader<S> {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        let new_pos: i128 = match pos {
            SeekFrom::Start(p) => p as i128,
            SeekFrom::Current(delta) => (self.pos as i128).saturating_add(delta as i128),
            SeekFrom::End(delta) => {
                let Some(len) = self.source.len() else {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::Unsupported,
                        StreamError::<S::Error>::UnknownLength.to_string(),
                    ));
                };
                (len as i128).saturating_add(delta as i128)
            }
        };

        if new_pos < 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                StreamError::<S::Error>::InvalidSeek.to_string(),
            ));
        }

        let new_pos_u64 = new_pos as u64;

        if let Some(len) = self.source.len()
            && new_pos_u64 > len
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                StreamError::<S::Error>::InvalidSeek.to_string(),
            ));
        }

        self.pos = new_pos_u64;
        Ok(self.pos)
    }
}

#[cfg(test)]
mod tests {
    use std::ops::Range;

    use kithara_stream::{MediaInfo, Source, StreamResult, WaitOutcome};

    use super::*;

    // MockSource — in-memory Source for testing SourceReader

    struct MockSource {
        data: Vec<u8>,
        media_info: Option<MediaInfo>,
    }

    impl MockSource {
        fn new(data: Vec<u8>) -> Self {
            Self {
                data,
                media_info: None,
            }
        }

        fn with_media_info(mut self, info: MediaInfo) -> Self {
            self.media_info = Some(info);
            self
        }
    }

    impl Source for MockSource {
        type Item = u8;
        type Error = std::io::Error;

        fn wait_range(&mut self, _range: Range<u64>) -> StreamResult<WaitOutcome, Self::Error> {
            Ok(WaitOutcome::Ready)
        }

        fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> StreamResult<usize, Self::Error> {
            let offset = offset as usize;
            if offset >= self.data.len() {
                return Ok(0);
            }
            let available = &self.data[offset..];
            let n = available.len().min(buf.len());
            buf[..n].copy_from_slice(&available[..n]);
            Ok(n)
        }

        fn len(&self) -> Option<u64> {
            Some(self.data.len() as u64)
        }

        fn media_info(&self) -> Option<MediaInfo> {
            self.media_info.clone()
        }
    }

    /// Source that returns Eof from `wait_range` for offsets beyond its data.
    struct EofSource {
        data: Vec<u8>,
    }

    impl EofSource {
        fn new(data: Vec<u8>) -> Self {
            Self { data }
        }
    }

    impl Source for EofSource {
        type Item = u8;
        type Error = std::io::Error;

        fn wait_range(&mut self, range: Range<u64>) -> StreamResult<WaitOutcome, Self::Error> {
            if range.start >= self.data.len() as u64 {
                Ok(WaitOutcome::Eof)
            } else {
                Ok(WaitOutcome::Ready)
            }
        }

        fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> StreamResult<usize, Self::Error> {
            let offset = offset as usize;
            if offset >= self.data.len() {
                return Ok(0);
            }
            let available = &self.data[offset..];
            let n = available.len().min(buf.len());
            buf[..n].copy_from_slice(&available[..n]);
            Ok(n)
        }

        fn len(&self) -> Option<u64> {
            Some(self.data.len() as u64)
        }
    }

    /// Source with unknown length (`len()` returns None).
    struct UnknownLenSource;

    impl Source for UnknownLenSource {
        type Item = u8;
        type Error = std::io::Error;

        fn wait_range(&mut self, _range: Range<u64>) -> StreamResult<WaitOutcome, Self::Error> {
            Ok(WaitOutcome::Ready)
        }

        fn read_at(&mut self, _offset: u64, _buf: &mut [u8]) -> StreamResult<usize, Self::Error> {
            Ok(0)
        }

        fn len(&self) -> Option<u64> {
            None
        }
    }

    // Helpers

    fn test_data() -> Vec<u8> {
        (0u8..=255).cycle().take(1024).collect()
    }

    fn make_reader(data: Vec<u8>) -> SourceReader<MockSource> {
        SourceReader::new(MockSource::new(data))
    }

    // Tests

    #[test]
    fn test_source_reader_read() {
        let data = test_data();
        let mut reader = make_reader(data.clone());

        let mut buf = vec![0u8; 64];
        let n = reader.read(&mut buf).unwrap();

        assert_eq!(n, 64);
        assert_eq!(&buf[..n], &data[..64]);
    }

    #[test]
    fn test_source_reader_sequential_reads() {
        let data = test_data();
        let mut reader = make_reader(data.clone());

        // First read: 100 bytes from offset 0
        let mut buf1 = vec![0u8; 100];
        let n1 = reader.read(&mut buf1).unwrap();
        assert_eq!(n1, 100);
        assert_eq!(&buf1[..n1], &data[..100]);
        assert_eq!(reader.position(), 100);

        // Second read: 100 bytes from offset 100
        let mut buf2 = vec![0u8; 100];
        let n2 = reader.read(&mut buf2).unwrap();
        assert_eq!(n2, 100);
        assert_eq!(&buf2[..n2], &data[100..200]);
        assert_eq!(reader.position(), 200);

        // Third read: 50 bytes from offset 200
        let mut buf3 = vec![0u8; 50];
        let n3 = reader.read(&mut buf3).unwrap();
        assert_eq!(n3, 50);
        assert_eq!(&buf3[..n3], &data[200..250]);
        assert_eq!(reader.position(), 250);
    }

    #[test]
    fn test_source_reader_seek_start() {
        let data = test_data();
        let mut reader = make_reader(data.clone());

        // Seek to offset 500
        let pos = reader.seek(SeekFrom::Start(500)).unwrap();
        assert_eq!(pos, 500);
        assert_eq!(reader.position(), 500);

        // Read from that position
        let mut buf = vec![0u8; 32];
        let n = reader.read(&mut buf).unwrap();
        assert_eq!(n, 32);
        assert_eq!(&buf[..n], &data[500..532]);
    }

    #[test]
    fn test_source_reader_seek_current() {
        let data = test_data();
        let mut reader = make_reader(data.clone());

        // Read 100 bytes to advance position
        let mut discard = vec![0u8; 100];
        reader.read_exact(&mut discard).unwrap();
        assert_eq!(reader.position(), 100);

        // Seek +50 from current (100 + 50 = 150)
        let pos = reader.seek(SeekFrom::Current(50)).unwrap();
        assert_eq!(pos, 150);

        // Read from position 150
        let mut buf = vec![0u8; 16];
        let n = reader.read(&mut buf).unwrap();
        assert_eq!(n, 16);
        assert_eq!(&buf[..n], &data[150..166]);

        // Seek -20 from current (166 - 20 = 146)
        let pos = reader.seek(SeekFrom::Current(-20)).unwrap();
        assert_eq!(pos, 146);

        let mut buf = vec![0u8; 8];
        let n = reader.read(&mut buf).unwrap();
        assert_eq!(n, 8);
        assert_eq!(&buf[..n], &data[146..154]);
    }

    #[test]
    fn test_source_reader_seek_end() {
        let data = test_data();
        let data_len = data.len() as u64;
        let mut reader = make_reader(data.clone());

        // Seek to 100 bytes before end
        let pos = reader.seek(SeekFrom::End(-100)).unwrap();
        assert_eq!(pos, data_len - 100);

        // Read those last 100 bytes
        let mut buf = vec![0u8; 100];
        let n = reader.read(&mut buf).unwrap();
        assert_eq!(n, 100);
        assert_eq!(&buf[..n], &data[(data_len as usize - 100)..]);
    }

    #[test]
    fn test_source_reader_read_at_eof() {
        let data = vec![1u8, 2, 3, 4, 5];
        let mut reader = SourceReader::new(EofSource::new(data));

        // Seek to end
        reader.seek(SeekFrom::Start(5)).unwrap();

        // Read past EOF — wait_range returns Eof, so read returns 0
        let mut buf = vec![0u8; 64];
        let n = reader.read(&mut buf).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn test_source_reader_seek_then_read() {
        let data = test_data();
        let mut reader = make_reader(data.clone());

        // Seek to middle
        let mid = data.len() / 2;
        reader.seek(SeekFrom::Start(mid as u64)).unwrap();

        // Read from middle
        let mut buf = vec![0u8; 64];
        let n = reader.read(&mut buf).unwrap();
        assert_eq!(n, 64);
        assert_eq!(&buf[..n], &data[mid..mid + 64]);
    }

    #[test]
    fn test_source_reader_read_empty_buf() {
        let data = test_data();
        let mut reader = make_reader(data);

        // Read with empty buffer returns 0 immediately
        let mut buf = vec![];
        let n = reader.read(&mut buf).unwrap();
        assert_eq!(n, 0);
        assert_eq!(reader.position(), 0, "Position should not advance");
    }

    #[test]
    fn test_source_reader_seek_beyond_end_fails() {
        let data = vec![0u8; 100];
        let mut reader = make_reader(data);

        // Seek past the end (len is 100, seeking to 101 should fail)
        let result = reader.seek(SeekFrom::Start(101));
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::InvalidInput);
    }

    #[test]
    fn test_source_reader_seek_negative_fails() {
        let data = vec![0u8; 100];
        let mut reader = make_reader(data);

        // Seek to negative position from current (pos=0, delta=-1)
        let result = reader.seek(SeekFrom::Current(-1));
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::InvalidInput);
    }

    #[test]
    fn test_source_reader_seek_end_unknown_length() {
        let mut reader = SourceReader::new(UnknownLenSource);

        // SeekFrom::End requires known length
        let result = reader.seek(SeekFrom::End(-10));
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::Unsupported);
    }

    #[test]
    fn test_source_reader_seek_to_exact_end() {
        let data = vec![0u8; 100];
        let mut reader = make_reader(data);

        // Seeking to exact end (len) should succeed
        let pos = reader.seek(SeekFrom::Start(100)).unwrap();
        assert_eq!(pos, 100);
    }

    #[test]
    fn test_source_reader_seek_end_zero_delta() {
        let data = vec![0u8; 100];
        let mut reader = make_reader(data);

        // SeekFrom::End(0) should position at exact end
        let pos = reader.seek(SeekFrom::End(0)).unwrap();
        assert_eq!(pos, 100);
    }

    #[test]
    fn test_source_reader_position_starts_at_zero() {
        let data = test_data();
        let reader = make_reader(data);
        assert_eq!(reader.position(), 0);
    }

    #[test]
    fn test_source_reader_media_info() {
        let info = MediaInfo::default()
            .with_sample_rate(44100)
            .with_channels(2);
        let source = MockSource::new(vec![0u8; 10]).with_media_info(info.clone());
        let reader = SourceReader::new(source);

        assert_eq!(reader.media_info(), Some(info));
    }

    #[test]
    fn test_source_reader_media_info_none() {
        let source = MockSource::new(vec![0u8; 10]);
        let reader = SourceReader::new(source);

        assert_eq!(reader.media_info(), None);
    }

    #[test]
    fn test_source_reader_read_partial_at_end() {
        let data = vec![10u8, 20, 30, 40, 50];
        let mut reader = make_reader(data.clone());

        // Seek to offset 3
        reader.seek(SeekFrom::Start(3)).unwrap();

        // Ask for 64 bytes, but only 2 remain
        let mut buf = vec![0u8; 64];
        let n = reader.read(&mut buf).unwrap();
        assert_eq!(n, 2);
        assert_eq!(&buf[..n], &data[3..5]);
    }

    #[test]
    fn test_source_reader_read_entire_source() {
        let data: Vec<u8> = (0..=99).collect();
        let mut reader = make_reader(data.clone());

        let mut result = Vec::new();
        let mut buf = vec![0u8; 30];
        loop {
            let n = reader.read(&mut buf).unwrap();
            if n == 0 {
                break;
            }
            result.extend_from_slice(&buf[..n]);
        }

        assert_eq!(result, data);
    }
}
