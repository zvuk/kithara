use crate::render::{WaveBucket, WaveformView};

#[derive(PartialEq)]
pub(crate) struct WaveformData {
    pub(crate) buckets: Box<[WaveBucket]>,
    pub(crate) revision: u64,
    pub(crate) beats: Box<[f32]>,
    pub(crate) downbeats: Box<[f32]>,
    pub(crate) loop_region: Option<[f32; 2]>,
    pub(crate) cues: Box<[f32]>,
}

impl From<WaveformView<'_>> for WaveformData {
    fn from(view: WaveformView<'_>) -> Self {
        Self {
            buckets: view.buckets.to_vec().into_boxed_slice(),
            revision: view.revision,
            beats: view.beats.to_vec().into_boxed_slice(),
            downbeats: view.downbeats.to_vec().into_boxed_slice(),
            loop_region: view.r#loop,
            cues: view.cues.to_vec().into_boxed_slice(),
        }
    }
}

impl WaveformData {
    /// Whether the copy already held is the frame just read.
    ///
    /// A track's buckets run to six figures, and a deck asks this on every
    /// frame of every deck it shows, so the buckets are judged by the name
    /// their owner gave them rather than compared. The marks are a handful
    /// of floats from a different producer, and are still read as they are.
    #[cfg(any(feature = "masonry", test))]
    pub(crate) fn matches(&self, view: WaveformView<'_>) -> bool {
        self.revision == view.revision
            && self.beats.as_ref() == view.beats
            && self.downbeats.as_ref() == view.downbeats
            && self.loop_region == view.r#loop
            && self.cues.as_ref() == view.cues
    }
}

#[cfg(test)]
mod tests {
    use super::{WaveformData, WaveformView};
    use crate::render::WaveBucket;

    fn bucket(high: f32) -> WaveBucket {
        WaveBucket {
            high,
            low: 1.0 - high,
            mid: 0.5,
        }
    }

    fn view<'a>(buckets: &'a [WaveBucket], revision: u64, beats: &'a [f32]) -> WaveformView<'a> {
        WaveformView {
            buckets,
            revision,
            beats,
            cues: &[],
            downbeats: &[],
            bpm: None,
            r#loop: None,
        }
    }

    #[test]
    fn a_new_name_is_a_new_waveform_even_when_the_buckets_read_the_same() {
        let held = WaveformData::from(view(&[bucket(0.9)], 7, &[]));

        assert!(!held.matches(view(&[bucket(0.9)], 8, &[])));
    }

    #[test]
    fn the_copy_takes_the_name_its_owner_gave_the_buckets_on_trust() {
        let held = WaveformData::from(view(&[bucket(0.9)], 7, &[]));

        assert!(held.matches(view(&[bucket(0.2)], 7, &[])));
    }

    #[test]
    fn marks_that_move_under_an_unmoved_name_still_land() {
        let held = WaveformData::from(view(&[bucket(0.9)], 7, &[0.25]));

        assert!(!held.matches(view(&[bucket(0.9)], 7, &[0.25, 0.5])));
    }
}

#[derive(PartialEq)]
pub(crate) struct OverlayData {
    pub(crate) title: String,
    pub(crate) artist: String,
    pub(crate) bpm: String,
    pub(crate) key: String,
    pub(crate) remain: String,
    pub(crate) badge: String,
}
