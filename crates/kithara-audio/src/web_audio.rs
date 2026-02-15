//! WASM audio output via `AudioWorklet`.
//!
//! Provides a PCM ring buffer that bridges Rust decode output to a JS
//! `AudioWorklet` processor via `SharedArrayBuffer` (WASM linear memory).

use std::sync::atomic::{AtomicU32, Ordering};

/// Default PCM ring buffer capacity: 16384 samples (~0.37s at 44100 Hz stereo).
const DEFAULT_PCM_RING_CAPACITY: usize = 16384;

/// PCM ring buffer for `AudioWorklet` consumption.
///
/// Single-producer (Rust decode loop writes via [`write()`](Self::write)),
/// single-consumer (JS `AudioWorklet` reads via `SharedArrayBuffer`).
///
/// Synchronization: two [`AtomicU32`] counters (`write_head`, `read_head`).
/// JS uses `Atomics.load`/`Atomics.store` on these.
pub struct PcmRingBuffer {
    /// Interleaved f32 samples (power-of-2 length).
    buf: Vec<f32>,
    /// Write position (sample index, monotonically increasing, wraps via mask).
    write_head: AtomicU32,
    /// Read position (sample index, monotonically increasing, wraps via mask).
    read_head: AtomicU32,
    /// Capacity mask `(capacity - 1)`.
    mask: u32,
    /// Number of channels.
    channels: u32,
    /// Sample rate in Hz.
    sample_rate: u32,
}

impl PcmRingBuffer {
    /// Create a new PCM ring buffer.
    ///
    /// `capacity` is rounded up to the next power of 2 (minimum 1024).
    #[must_use]
    pub fn new(capacity: usize, channels: u32, sample_rate: u32) -> Self {
        let capacity = capacity.next_power_of_two().max(1024);
        let mut buf = Vec::with_capacity(capacity);
        buf.resize(capacity, 0.0);
        Self {
            buf,
            write_head: AtomicU32::new(0),
            read_head: AtomicU32::new(0),
            #[expect(clippy::cast_possible_truncation)]
            mask: (capacity - 1) as u32,
            channels,
            sample_rate,
        }
    }

    /// Create with default capacity (16384 samples).
    #[must_use]
    pub fn with_defaults(channels: u32, sample_rate: u32) -> Self {
        Self::new(DEFAULT_PCM_RING_CAPACITY, channels, sample_rate)
    }

    /// Write interleaved PCM samples into the ring buffer.
    ///
    /// Returns number of samples actually written (less if buffer full).
    /// Call this from the decode loop.
    pub fn write(&mut self, samples: &[f32]) -> usize {
        let write = self.write_head.load(Ordering::Acquire);
        let read = self.read_head.load(Ordering::Acquire);

        let used = write.wrapping_sub(read);
        let available = (self.mask + 1) - used;
        let to_write = samples.len().min(available as usize);

        if to_write == 0 {
            return 0;
        }

        for (i, &sample) in samples[..to_write].iter().enumerate() {
            #[expect(clippy::cast_possible_truncation)]
            let idx = (write.wrapping_add(i as u32)) & self.mask;
            self.buf[idx as usize] = sample;
        }

        #[expect(clippy::cast_possible_truncation)]
        let advance = to_write as u32;
        self.write_head
            .store(write.wrapping_add(advance), Ordering::Release);
        to_write
    }

    /// Pointer to the f32 buffer data (for JS `SharedArrayBuffer` view).
    #[must_use]
    pub fn buf_ptr(&self) -> *const f32 {
        self.buf.as_ptr()
    }

    /// Buffer capacity in samples.
    #[must_use]
    pub fn capacity(&self) -> u32 {
        self.mask + 1
    }

    /// Pointer to `write_head` atomic (for JS `Atomics.load`).
    #[must_use]
    pub fn write_head_ptr(&self) -> *const AtomicU32 {
        &self.write_head
    }

    /// Pointer to `read_head` atomic (for JS `Atomics.store`).
    #[must_use]
    pub fn read_head_ptr(&self) -> *const AtomicU32 {
        &self.read_head
    }

    /// Number of channels.
    #[must_use]
    pub fn channels(&self) -> u32 {
        self.channels
    }

    /// Sample rate in Hz.
    #[must_use]
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Reset both heads to 0 (e.g., on seek or stop).
    pub fn reset(&mut self) {
        self.write_head.store(0, Ordering::Release);
        self.read_head.store(0, Ordering::Release);
    }

    /// Number of samples currently available for reading.
    #[must_use]
    pub fn available(&self) -> u32 {
        let write = self.write_head.load(Ordering::Acquire);
        let read = self.read_head.load(Ordering::Acquire);
        write.wrapping_sub(read)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_rounds_to_power_of_two() {
        let ring = PcmRingBuffer::new(1000, 2, 44100);
        assert_eq!(ring.capacity(), 1024);
    }

    #[test]
    fn new_enforces_minimum_capacity() {
        let ring = PcmRingBuffer::new(16, 2, 44100);
        assert_eq!(ring.capacity(), 1024);
    }

    #[test]
    fn write_and_available() {
        let mut ring = PcmRingBuffer::new(1024, 2, 44100);
        assert_eq!(ring.available(), 0);

        let samples = vec![0.5f32; 100];
        let written = ring.write(&samples);
        assert_eq!(written, 100);
        assert_eq!(ring.available(), 100);
    }

    #[test]
    fn write_full_buffer() {
        let mut ring = PcmRingBuffer::new(1024, 2, 44100);
        let samples = vec![0.1f32; 2000];
        let written = ring.write(&samples);
        assert_eq!(written, 1024);
        assert_eq!(ring.available(), 1024);

        // Buffer full -- writing more returns 0.
        let written = ring.write(&[0.2f32; 10]);
        assert_eq!(written, 0);
    }

    #[test]
    fn reset_clears_heads() {
        let mut ring = PcmRingBuffer::new(1024, 2, 44100);
        ring.write(&[0.1f32; 500]);
        assert_eq!(ring.available(), 500);

        ring.reset();
        assert_eq!(ring.available(), 0);
    }

    #[test]
    fn with_defaults_uses_expected_capacity() {
        let ring = PcmRingBuffer::with_defaults(2, 48000);
        assert_eq!(ring.capacity(), 16384);
        assert_eq!(ring.channels(), 2);
        assert_eq!(ring.sample_rate(), 48000);
    }

    #[test]
    fn write_wraps_around_correctly() {
        let mut ring = PcmRingBuffer::new(1024, 1, 44100);

        // Fill completely.
        ring.write(&vec![1.0f32; 1024]);
        assert_eq!(ring.available(), 1024);

        // Simulate consumer reading 512 samples by advancing read_head.
        ring.read_head.store(512, Ordering::Release);
        assert_eq!(ring.available(), 512);

        // Write 512 more -- should wrap around.
        let samples: Vec<f32> = (0..512).map(|i| i as f32 * 0.001).collect();
        let written = ring.write(&samples);
        assert_eq!(written, 512);
        assert_eq!(ring.available(), 1024);
    }

    #[test]
    fn pointers_are_non_null() {
        let ring = PcmRingBuffer::new(1024, 2, 44100);
        assert!(!ring.buf_ptr().is_null());
        assert!(!ring.write_head_ptr().is_null());
        assert!(!ring.read_head_ptr().is_null());
    }

    #[test]
    fn write_empty_slice_returns_zero() {
        let mut ring = PcmRingBuffer::new(1024, 2, 44100);
        let written = ring.write(&[]);
        assert_eq!(written, 0);
        assert_eq!(ring.available(), 0);
    }

    #[test]
    fn written_data_is_readable_at_correct_indices() {
        let mut ring = PcmRingBuffer::new(1024, 2, 44100);
        let samples: Vec<f32> = (0..10).map(|i| i as f32 * 0.1).collect();
        ring.write(&samples);

        // Verify data was written correctly.
        for (i, &expected) in samples.iter().enumerate() {
            assert!(
                (ring.buf[i] - expected).abs() < f32::EPSILON,
                "sample {i}: expected {expected}, got {}",
                ring.buf[i]
            );
        }
    }
}
