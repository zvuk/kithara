use std::{collections::VecDeque, time::Duration};

/// Tracks buffer level for ABR decisions.
///
/// Maintains a queue of segment durations and tracks playback position
/// to calculate how much content is buffered ahead of playback.
#[derive(Debug, Clone)]
pub struct BufferTracker {
    segments: VecDeque<Duration>,
    playback_position: Duration,
}

impl BufferTracker {
    /// Create a new buffer tracker.
    pub fn new() -> Self {
        Self {
            segments: VecDeque::new(),
            playback_position: Duration::ZERO,
        }
    }

    /// Get buffer level in seconds.
    ///
    /// Returns the amount of content buffered ahead of playback position.
    pub fn buffer_level_secs(&self) -> f64 {
        let total: Duration = self.segments.iter().sum();
        total
            .checked_sub(self.playback_position)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0)
    }

    /// Add a segment to the buffer.
    pub fn add_segment(&mut self, duration: Duration) {
        self.segments.push_back(duration);
    }

    /// Advance playback position and remove consumed segments.
    pub fn advance_playback(&mut self, elapsed: Duration) {
        self.playback_position += elapsed;

        while let Some(&front) = self.segments.front() {
            if self.playback_position >= front {
                self.playback_position -= front;
                self.segments.pop_front();
            } else {
                break;
            }
        }
    }

    /// Reset the buffer tracker.
    pub fn reset(&mut self) {
        self.segments.clear();
        self.playback_position = Duration::ZERO;
    }

    /// Get total buffered duration.
    pub fn total_buffered(&self) -> Duration {
        self.segments.iter().sum()
    }

    /// Get number of segments in buffer.
    pub fn segment_count(&self) -> usize {
        self.segments.len()
    }
}

impl Default for BufferTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_initial_state() {
        let tracker = BufferTracker::new();
        assert_eq!(tracker.buffer_level_secs(), 0.0);
        assert_eq!(tracker.total_buffered(), Duration::ZERO);
        assert_eq!(tracker.segment_count(), 0);
    }

    #[test]
    fn test_add_segment() {
        let mut tracker = BufferTracker::new();

        tracker.add_segment(Duration::from_secs(4));
        assert_eq!(tracker.buffer_level_secs(), 4.0);
        assert_eq!(tracker.segment_count(), 1);

        tracker.add_segment(Duration::from_secs(4));
        assert_eq!(tracker.buffer_level_secs(), 8.0);
        assert_eq!(tracker.segment_count(), 2);
    }

    #[test]
    fn test_advance_playback() {
        let mut tracker = BufferTracker::new();

        tracker.add_segment(Duration::from_secs(4));
        tracker.add_segment(Duration::from_secs(4));

        tracker.advance_playback(Duration::from_secs(2));
        assert_eq!(tracker.buffer_level_secs(), 6.0);
        assert_eq!(tracker.segment_count(), 2);

        tracker.advance_playback(Duration::from_secs(3));
        assert_eq!(tracker.buffer_level_secs(), 3.0);
        assert_eq!(tracker.segment_count(), 1);
    }

    #[test]
    fn test_advance_playback_consumes_segments() {
        let mut tracker = BufferTracker::new();

        tracker.add_segment(Duration::from_secs(4));
        tracker.add_segment(Duration::from_secs(4));

        tracker.advance_playback(Duration::from_secs(5));
        assert_eq!(tracker.segment_count(), 1);
        assert_eq!(tracker.buffer_level_secs(), 3.0);
    }

    #[test]
    fn test_reset() {
        let mut tracker = BufferTracker::new();

        tracker.add_segment(Duration::from_secs(4));
        tracker.advance_playback(Duration::from_secs(2));

        tracker.reset();
        assert_eq!(tracker.buffer_level_secs(), 0.0);
        assert_eq!(tracker.segment_count(), 0);
        assert_eq!(tracker.total_buffered(), Duration::ZERO);
    }

    #[test]
    fn test_buffer_level_never_negative() {
        let mut tracker = BufferTracker::new();

        tracker.add_segment(Duration::from_secs(4));
        tracker.advance_playback(Duration::from_secs(10));

        assert_eq!(tracker.buffer_level_secs(), 0.0);
    }

    #[test]
    fn test_default_impl() {
        let tracker = BufferTracker::default();
        assert_eq!(tracker.segment_count(), 0);
    }
}
