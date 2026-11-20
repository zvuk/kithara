use num_traits::cast::AsPrimitive;

use crate::{
    atoms::wave::{
        face::{Drawn, cached_extent},
        snapshot::{OverlayData, WaveformData},
    },
    hosts::model::derived,
    render::{ReadValue, Reads, WaveformView, Zoom},
};

impl Drawn {
    pub(crate) fn refresh(&mut self, reads: &dyn Reads, scope: &str, zoom: Option<&str>) -> bool {
        let progress = match reads.get(&derived("deck.playback.position_normalized", scope)) {
            Some(ReadValue::Scalar(value)) => value.as_(),
            _ => 0.0,
        };
        let zoom = zoom
            .and_then(|endpoint| reads.get(endpoint))
            .and_then(|value| match value {
                ReadValue::Scalar(value) => Some(AsPrimitive::<f32>::as_(value)),
                _ => None,
            })
            .map_or(self.zoom, Zoom::from);
        let cached = cached_extent(reads, scope, progress);
        let mut changed = std::mem::replace(&mut self.progress, progress) != progress;
        changed |= std::mem::replace(&mut self.cached, cached) != cached;
        changed |= std::mem::replace(&mut self.zoom, zoom) != zoom;
        if let Some(overlay) = &mut self.overlay {
            let next = OverlayData::read(reads, scope, overlay.bpm.clone(), overlay.badge.clone());
            changed |= std::mem::replace(overlay, next) != *overlay;
        }
        changed
    }

    pub(crate) fn set_waveform(&mut self, view: WaveformView<'_>) -> bool {
        let waveform_changed = self
            .waveform
            .as_ref()
            .is_none_or(|waveform| !waveform.matches(view));
        if waveform_changed {
            self.waveform = Some(WaveformData::from(view));
        }
        let bpm = view
            .bpm
            .map_or_else(|| Self::EM_DASH.to_owned(), |value| format!("{value:.2}"));
        let bpm_changed = self.overlay.as_mut().is_some_and(|overlay| {
            bpm != overlay.bpm && {
                overlay.bpm = bpm;
                true
            }
        });
        waveform_changed || bpm_changed
    }
}

impl WaveformData {
    /// Whether the copy already held is the frame just read.
    ///
    /// A track's buckets run to six figures, and a deck asks this on every
    /// frame of every deck it shows, so the buckets are judged by the name
    /// their owner gave them rather than compared. The marks are a handful
    /// of floats from a different producer, and are still read as they are.
    pub(crate) fn matches(&self, view: WaveformView<'_>) -> bool {
        self.revision == view.revision
            && self.beats.as_ref() == view.beats
            && self.downbeats.as_ref() == view.downbeats
            && self.unready.as_ref() == view.unready
            && self.loop_region == view.r#loop
            && self.cues.as_ref() == view.cues
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use crate::{
        atoms::wave::{
            face::{
                Drawn,
                tests::{ArtReads, art, hero, reads},
            },
            snapshot::WaveformData,
        },
        builtin,
        module::WaveStyle,
        render::{ReadValue, Reads, WaveBucket, WaveformView},
    };

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
            unready: &[],
            bpm: None,
            r#loop: None,
        }
    }

    #[kithara::test]
    fn a_new_name_is_a_new_waveform_even_when_the_buckets_read_the_same() {
        let held = WaveformData::from(view(&[bucket(0.9)], 7, &[]));

        assert!(!held.matches(view(&[bucket(0.9)], 8, &[])));
    }

    #[kithara::test]
    fn the_copy_takes_the_name_its_owner_gave_the_buckets_on_trust() {
        let held = WaveformData::from(view(&[bucket(0.9)], 7, &[]));

        assert!(held.matches(view(&[bucket(0.2)], 7, &[])));
    }

    #[kithara::test]
    fn marks_that_move_under_an_unmoved_name_still_land() {
        let held = WaveformData::from(view(&[bucket(0.9)], 7, &[0.25]));

        assert!(!held.matches(view(&[bucket(0.9)], 7, &[0.25, 0.5])));
    }

    /// Coverage arriving under an unmoved name must still reach the retained
    /// host: a growing analysis would otherwise keep the picture it was first
    /// drawn with, and nothing else reports the staleness.
    #[kithara::test]
    fn coverage_that_grows_is_a_waveform_the_copy_does_not_match() {
        let held = WaveformData::from(WaveformView {
            unready: &[[0.2, 0.4]],
            ..view(&[bucket(0.9)], 7, &[])
        });

        assert!(!held.matches(WaveformView {
            unready: &[[0.3, 0.4]],
            ..view(&[bucket(0.9)], 7, &[])
        }));
    }

    struct UpdatedReads;

    impl Reads for UpdatedReads {
        fn get(&self, endpoint: &str) -> Option<ReadValue<'_>> {
            match endpoint {
                "deck.playback.position_normalized@deck=a" => Some(ReadValue::Scalar(0.75)),
                "deck.playback.remain@deck=a" => Some(ReadValue::Text("-00:15")),
                "deck.track.key@deck=a" => Some(ReadValue::Text("9m")),
                "deck.track.source_kind@deck=a" => Some(ReadValue::Text("file")),
                "deck.track.title@deck=a" => Some(ReadValue::Text("Updated")),
                "deck.waveform.zoom@deck=a" => Some(ReadValue::Scalar(2.0)),
                _ => None,
            }
        }
    }

    /// A retained wave owns all scoped words around its primary waveform, so
    /// analysis and playback updates do not need a document rebuild.
    #[kithara::test]
    fn a_retained_wave_refreshes_its_scoped_snapshot() {
        let skin = builtin::skin();
        let (_, mut data) = hero(skin);

        assert!(data.refresh(&UpdatedReads, "@deck=a", Some("deck.waveform.zoom@deck=a")));

        let overlay = data
            .overlay
            .as_ref()
            .expect("a hero wave must keep its naming panel");
        assert_eq!(overlay.title, "Updated");
        assert_eq!(overlay.artist, "file");
        assert_eq!(overlay.key, "9m");
        assert_eq!(overlay.remain, "-00:15");
        assert_eq!(data.progress, 0.75);
        assert_eq!(f32::from(data.zoom), 0.5);

        struct EmptyKeyReads;

        impl Reads for EmptyKeyReads {
            fn get(&self, endpoint: &str) -> Option<ReadValue<'_>> {
                (endpoint == "deck.track.key@deck=a").then_some(ReadValue::Text(""))
            }
        }

        assert!(data.refresh(&EmptyKeyReads, "@deck=a", None));
        assert_eq!(
            data.overlay.as_ref().map(|overlay| overlay.key.as_str()),
            Some(Drawn::EM_DASH)
        );
    }

    #[kithara::test]
    fn artwork_arrival_and_removal_update_both_wave_paths() {
        let mut reads = ArtReads(None);
        let mut retained = Drawn::read(WaveStyle::Hero, 1.0, Some("A"), None, &reads, "@deck=a");
        assert!(!retained.refresh(&reads, "@deck=a", None));
        for next in [Some(art("cover", 4, 2)), None] {
            reads.0 = next;
            assert!(retained.refresh(&reads, "@deck=a", None));
            let immediate = Drawn::read(WaveStyle::Hero, 1.0, Some("A"), None, &reads, "@deck=a");
            assert!(retained == immediate);
            assert_eq!(
                retained.overlay.as_ref().expect("hero overlay").art,
                reads.0
            );
            assert!(!retained.refresh(&reads, "@deck=a", None));
            let other = Drawn::read(WaveStyle::Hero, 1.0, Some("B"), None, &reads, "@deck=b");
            assert!(
                other
                    .overlay
                    .as_ref()
                    .expect("other hero overlay")
                    .art
                    .is_none()
            );
        }
    }

    /// The continuously repainted wave keeps its owned sample arrays when the
    /// borrowed view has not changed, while still taking a new BPM reading.
    #[kithara::test]
    fn an_unchanged_waveform_is_not_copied_each_frame() {
        let skin = builtin::skin();
        let reads = reads();
        let (_, mut data) = hero(skin);
        let buckets = data
            .waveform
            .as_ref()
            .expect("the fixture must own waveform samples")
            .buckets
            .as_ptr();
        let value = reads
            .get("deck.playback.waveform")
            .expect("the fixture must report a waveform");
        let ReadValue::Waveform(mut view) = value else {
            panic!("the waveform endpoint must report a waveform");
        };

        assert!(!data.set_waveform(view));
        assert_eq!(
            data.waveform
                .as_ref()
                .map(|waveform| waveform.buckets.as_ptr()),
            Some(buckets)
        );
        view.bpm = Some(129.0);
        assert!(data.set_waveform(view));
        assert_eq!(
            data.waveform
                .as_ref()
                .map(|waveform| waveform.buckets.as_ptr()),
            Some(buckets)
        );
        assert_eq!(
            data.overlay.as_ref().map(|overlay| overlay.bpm.as_str()),
            Some("129.00")
        );
    }
}
