use crate::{AudioSource, DecodeCommand, DecodeResult, MediaSource, PcmChunk, PcmSpec, ReadSeek};

/// Test helper for MediaSource
#[derive(Clone)]
pub struct TestMediaSource {
    pub file_ext: Option<String>,
}

impl TestMediaSource {
    pub fn new(file_ext: &str) -> Self {
        Self {
            file_ext: Some(file_ext.to_string()),
        }
    }

    pub fn new_with_none() -> Self {
        Self { file_ext: None }
    }
}

impl MediaSource for TestMediaSource {
    fn reader(&self) -> Box<dyn ReadSeek + Send + Sync> {
        Box::new(std::io::empty())
    }

    fn file_ext(&self) -> Option<&str> {
        self.file_ext.as_deref()
    }
}

/// Fake audio source for testing AudioSource trait - f32 specific implementation
pub struct FakeAudioSourceF32 {
    spec: PcmSpec,
    current_frame: u64,
    total_frames: u64,
    chunk_size_frames: usize,
}

impl FakeAudioSourceF32 {
    pub fn new(spec: PcmSpec) -> Self {
        Self {
            spec,
            current_frame: 0,
            total_frames: 10000,    // Simulate 10k frames of content
            chunk_size_frames: 512, // 512 frames per chunk
        }
    }

    fn generate_samples(&self, start_frame: u64, frame_count: usize) -> Vec<f32> {
        let channels = self.spec.channels as usize;
        let total_samples = frame_count * channels;
        let mut samples = Vec::with_capacity(total_samples);

        for frame in start_frame..(start_frame + frame_count as u64) {
            for channel in 0..channels {
                // Generate a simple sine wave with frequency based on channel
                let sample_f32 = (frame as f32 * 0.01 + channel as f32 * 0.5).sin() * 0.5;
                samples.push(sample_f32);
            }
        }

        samples
    }
}

impl AudioSource<f32> for FakeAudioSourceF32 {
    fn output_spec(&self) -> Option<PcmSpec> {
        Some(self.spec)
    }

    fn next_chunk(&mut self) -> DecodeResult<Option<PcmChunk<f32>>> {
        if self.current_frame >= self.total_frames {
            return Ok(None);
        }

        let remaining_frames = (self.total_frames - self.current_frame) as usize;
        let frames_to_generate = std::cmp::min(self.chunk_size_frames, remaining_frames);

        let samples = self.generate_samples(self.current_frame, frames_to_generate);
        let chunk = PcmChunk::new(self.spec, samples);

        self.current_frame += frames_to_generate as u64;

        Ok(Some(chunk))
    }

    fn handle_command(&mut self, cmd: DecodeCommand) -> DecodeResult<()> {
        match cmd {
            DecodeCommand::Seek(pos) => {
                // Convert position to frames
                let seek_frames = ((pos.as_secs_f64() * self.spec.sample_rate as f64) as u64)
                    .min(self.total_frames);
                self.current_frame = seek_frames;
                Ok(())
            }
        }
    }
}

impl MediaSource for FakeAudioSourceF32 {
    fn reader(&self) -> Box<dyn ReadSeek + Send + Sync> {
        Box::new(std::io::empty())
    }

    fn file_ext(&self) -> Option<&str> {
        Some("wav") // Default extension for testing
    }
}

// Type alias for easier usage in tests
pub type FakeAudioSource = FakeAudioSourceF32;
