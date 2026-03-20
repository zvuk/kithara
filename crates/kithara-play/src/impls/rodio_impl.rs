//! `rodio::Source` implementation for [`Resource`].

use kithara_platform::time::Duration;

use super::resource::Resource;

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

    fn channels(&self) -> std::num::NonZeroU16 {
        std::num::NonZeroU16::new(self.inner.spec().channels).unwrap_or(std::num::NonZeroU16::MIN)
    }

    fn sample_rate(&self) -> std::num::NonZeroU32 {
        std::num::NonZeroU32::new(self.inner.spec().sample_rate)
            .unwrap_or(std::num::NonZeroU32::MIN)
    }

    fn total_duration(&self) -> Option<Duration> {
        self.inner.duration()
    }
}
