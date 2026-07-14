/// Little-endian append-only writer over a byte buffer.
pub(crate) struct Writer<'a>(&'a mut Vec<u8>);

impl<'a> Writer<'a> {
    pub(crate) fn new(bytes: &'a mut Vec<u8>) -> Self {
        Self(bytes)
    }

    pub(crate) fn reserve(&mut self, extra: usize) {
        self.0.reserve(extra);
    }

    pub(crate) fn write_f32(&mut self, value: f32) {
        self.0.extend_from_slice(&value.to_le_bytes());
    }

    pub(crate) fn write_f64(&mut self, value: f64) {
        self.0.extend_from_slice(&value.to_le_bytes());
    }

    /// Write a length-prefixed `u64` list.
    pub(crate) fn write_frames(&mut self, frames: &[u64]) {
        self.write_len(frames.len());
        for frame in frames {
            self.write_u64(*frame);
        }
    }

    /// Write a `u64` length prefix, clamping an oversized `usize` to `u64::MAX`
    /// (a length that always fails to read back).
    pub(crate) fn write_len(&mut self, len: usize) {
        self.write_u64(u64::try_from(len).unwrap_or(u64::MAX));
    }

    pub(crate) fn write_u32(&mut self, value: u32) {
        self.0.extend_from_slice(&value.to_le_bytes());
    }

    pub(crate) fn write_u64(&mut self, value: u64) {
        self.0.extend_from_slice(&value.to_le_bytes());
    }
}
