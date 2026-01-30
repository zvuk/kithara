//! rodio::Source implementation for [`Resource`].

use std::time::Duration;

use crate::resource::Resource;

impl Iterator for Resource {
    type Item = f32;

    fn next(&mut self) -> Option<Self::Item> {
        if self.inner.is_eof() {
            return None;
        }
        let mut sample = [0.0f32; 1];
        if self.inner.read(&mut sample) == 1 {
            Some(sample[0])
        } else {
            None
        }
    }
}

impl rodio::Source for Resource {
    fn current_span_len(&self) -> Option<usize> {
        None
    }

    fn channels(&self) -> u16 {
        self.inner.spec().channels
    }

    fn sample_rate(&self) -> u32 {
        self.inner.spec().sample_rate
    }

    fn total_duration(&self) -> Option<Duration> {
        self.inner.duration()
    }
}
