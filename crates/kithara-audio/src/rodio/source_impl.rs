//! `rodio::Source` implementation for [`Audio`].

use crate::pipeline::Audio;

impl<S> Iterator for Audio<S> {
    type Item = f32;

    fn next(&mut self) -> Option<Self::Item> {
        if self.eof {
            return None;
        }

        // Try to get sample from current chunk
        if let Some(ref chunk) = self.current_chunk
            && self.chunk_offset < chunk.pcm.len()
        {
            let sample = chunk.pcm[self.chunk_offset];
            self.chunk_offset += 1;
            self.timeline
                .advance_committed_samples(1, self.spec.sample_rate, self.spec.channels);
            return Some(sample);
        }

        // Chunk exhausted or no chunk - need more data
        if self.fill_buffer()
            && let Some(ref chunk) = self.current_chunk
            && self.chunk_offset < chunk.pcm.len()
        {
            let sample = chunk.pcm[self.chunk_offset];
            self.chunk_offset += 1;
            self.timeline
                .advance_committed_samples(1, self.spec.sample_rate, self.spec.channels);
            return Some(sample);
        }

        None
    }
}

impl<S> ::rodio::Source for Audio<S> {
    fn current_span_len(&self) -> Option<usize> {
        if let Some(ref chunk) = self.current_chunk
            && self.chunk_offset < chunk.pcm.len()
        {
            return Some(chunk.pcm.len() - self.chunk_offset);
        }
        None
    }

    fn channels(&self) -> u16 {
        self.spec.channels
    }

    fn sample_rate(&self) -> u32 {
        self.spec.sample_rate
    }

    fn total_duration(&self) -> Option<std::time::Duration> {
        self.timeline.total_duration()
    }

    fn try_seek(&mut self, pos: std::time::Duration) -> Result<(), ::rodio::source::SeekError> {
        self.seek(pos)
            .map_err(|err| ::rodio::source::SeekError::Other(Box::new(err)))
    }
}
