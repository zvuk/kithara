use std::collections::VecDeque;

use bytes::Bytes;
use kithara_platform::sync::Arc;

use crate::{BroadcastResult, config::BroadcastConfig, segment::Segment};

/// Value view of the live stream: rendered playlist plus the segments a client
/// can still fetch. Cloning shares both.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct PlaylistSnapshot {
    pub playlist: Arc<str>,
    pub segments: Arc<[Segment]>,
    pub is_finished: bool,
}

impl PlaylistSnapshot {
    /// Bytes of the retained segment `seq`.
    #[must_use]
    pub fn segment(&self, seq: u64) -> Option<Bytes> {
        self.segments
            .iter()
            .find(|segment| segment.seq == seq)
            .map(|segment| segment.bytes.clone())
    }
}

/// Sole owner of the live playlist window: it slides the window, retains
/// evicted segments for the grace, and renders the media playlist.
#[derive(Debug)]
pub struct LiveWindow {
    window: usize,
    retention: usize,
    timescale: u32,
    target_seconds: u64,
    discontinuity_sequence: u64,
    segments: VecDeque<Segment>,
    finished: bool,
}

impl LiveWindow {
    /// Open the window `config` describes.
    ///
    /// # Errors
    ///
    /// Returns [`BroadcastError::InvalidConfig`] or
    /// [`BroadcastError::PlaylistTooShort`] for a configuration the packager
    /// cannot serve.
    pub fn new(config: &BroadcastConfig) -> BroadcastResult<Self> {
        config.validate()?;

        Ok(Self {
            window: config.window,
            retention: config.window + config.grace,
            timescale: config.sample_rate,
            target_seconds: config.target_seconds()?,
            discontinuity_sequence: 0,
            segments: VecDeque::with_capacity(config.window + config.grace),
            finished: false,
        })
    }

    /// Append a closed segment, evicting whatever falls past the retention.
    pub fn push(&mut self, segment: Segment) {
        self.segments.push_back(segment);

        if self.segments.len() > self.window {
            let unlisted = self.segments.len() - self.window - 1;
            if self.segments[unlisted].discontinuity {
                self.discontinuity_sequence += 1;
            }
        }
        while self.segments.len() > self.retention {
            self.segments.pop_front();
        }
    }

    /// End the stream: the playlist gains `EXT-X-ENDLIST`.
    pub fn finish(&mut self) {
        self.finished = true;
    }

    /// Current playlist text and retained segments.
    #[must_use]
    pub fn snapshot(&self) -> PlaylistSnapshot {
        PlaylistSnapshot {
            playlist: Arc::from(self.render()),
            segments: self.segments.iter().cloned().collect(),
            is_finished: self.finished,
        }
    }

    fn render(&self) -> String {
        let evicted = self.segments.len().saturating_sub(self.window);
        let listed = self.segments.range(evicted..);
        let target = self.target_seconds;
        let media_sequence = listed.clone().next().map_or(0, |segment| segment.seq);
        let discontinuity_sequence = self.discontinuity_sequence;

        let mut playlist = format!(
            "#EXTM3U\n\
             #EXT-X-VERSION:3\n\
             #EXT-X-TARGETDURATION:{target}\n\
             #EXT-X-MEDIA-SEQUENCE:{media_sequence}\n\
             #EXT-X-DISCONTINUITY-SEQUENCE:{discontinuity_sequence}\n"
        );
        for segment in listed {
            if segment.discontinuity {
                playlist.push_str("#EXT-X-DISCONTINUITY\n");
            }
            let seconds = f64::from(segment.duration_ts) / f64::from(self.timescale);
            playlist.push_str(&format!("#EXTINF:{seconds:.3},\nseg/{}.aac\n", segment.seq));
        }
        if self.finished {
            playlist.push_str("#EXT-X-ENDLIST\n");
        }
        playlist
    }
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use kithara_platform::time::Duration;

    use super::LiveWindow;
    use crate::{config::BroadcastConfig, segment::Segment};

    struct Consts;

    impl Consts {
        const DURATION_TS: u32 = 192_512;
        const TIMESCALE: u32 = 48_000;
    }

    fn window() -> LiveWindow {
        LiveWindow::new(&BroadcastConfig::builder().build()).expect("window")
    }

    fn segment(seq: u64, discontinuity: bool) -> Segment {
        Segment {
            seq,
            bytes: Bytes::from(vec![u8::try_from(seq % 256).expect("fits"); 8]),
            duration_ts: Consts::DURATION_TS,
            discontinuity,
        }
    }

    fn fill(window: &mut LiveWindow, count: u64) {
        fill_from(window, 0, count);
    }

    fn fill_from(window: &mut LiveWindow, first: u64, count: u64) {
        for seq in first..first + count {
            window.push(segment(seq, false));
        }
    }

    fn listed() -> u64 {
        u64::try_from(BroadcastConfig::WINDOW).expect("the window fits a sequence number")
    }

    #[test]
    fn the_playlist_holds_the_last_window_of_segments() {
        let mut window = window();

        fill(&mut window, 10);

        assert_eq!(
            window.snapshot().playlist.as_ref(),
            "#EXTM3U\n\
             #EXT-X-VERSION:3\n\
             #EXT-X-TARGETDURATION:4\n\
             #EXT-X-MEDIA-SEQUENCE:4\n\
             #EXT-X-DISCONTINUITY-SEQUENCE:0\n\
             #EXTINF:4.011,\n\
             seg/4.aac\n\
             #EXTINF:4.011,\n\
             seg/5.aac\n\
             #EXTINF:4.011,\n\
             seg/6.aac\n\
             #EXTINF:4.011,\n\
             seg/7.aac\n\
             #EXTINF:4.011,\n\
             seg/8.aac\n\
             #EXTINF:4.011,\n\
             seg/9.aac\n"
        );
    }

    #[test]
    fn an_evicted_segment_stays_fetchable_for_the_grace() {
        let mut window = window();

        fill(&mut window, 10);
        let snapshot = window.snapshot();

        assert!(
            snapshot.segment(3).is_some(),
            "the segment evicted into the grace is still fetchable"
        );
        assert!(
            snapshot.segment(0).is_none(),
            "the segment past the grace is gone"
        );
        assert!(snapshot.segment(9).is_some());
    }

    #[test]
    fn a_discontinuous_segment_carries_the_tag() {
        let mut window = window();

        window.push(segment(0, false));
        window.push(segment(1, true));

        assert_eq!(
            window.snapshot().playlist.as_ref(),
            "#EXTM3U\n\
             #EXT-X-VERSION:3\n\
             #EXT-X-TARGETDURATION:4\n\
             #EXT-X-MEDIA-SEQUENCE:0\n\
             #EXT-X-DISCONTINUITY-SEQUENCE:0\n\
             #EXTINF:4.011,\n\
             seg/0.aac\n\
             #EXT-X-DISCONTINUITY\n\
             #EXTINF:4.011,\n\
             seg/1.aac\n"
        );
    }

    #[test]
    fn the_discontinuity_sequence_counts_the_tags_that_left_the_playlist() {
        let mut window = window();

        window.push(segment(0, true));
        fill_from(&mut window, 1, listed() - 1);
        assert!(
            window
                .snapshot()
                .playlist
                .contains("#EXT-X-DISCONTINUITY-SEQUENCE:0\n"),
            "a listed discontinuity is still the client's to see"
        );

        window.push(segment(listed(), false));
        let snapshot = window.snapshot();

        assert!(
            snapshot
                .playlist
                .contains("#EXT-X-DISCONTINUITY-SEQUENCE:1\n"),
            "{}",
            snapshot.playlist
        );
        assert!(
            !snapshot.playlist.contains("#EXT-X-DISCONTINUITY\n"),
            "the discontinuous segment left the playlist"
        );
    }

    #[test]
    fn a_live_playlist_has_no_endlist_and_a_finished_one_does() {
        let mut window = window();
        fill(&mut window, 3);

        let live = window.snapshot();
        window.finish();
        let finished = window.snapshot();

        assert!(!live.playlist.contains("#EXT-X-ENDLIST"));
        assert!(!live.is_finished);
        assert_eq!(
            finished.playlist.as_ref(),
            format!("{}#EXT-X-ENDLIST\n", live.playlist)
        );
        assert!(finished.is_finished);
        assert_eq!(
            window.snapshot().playlist,
            finished.playlist,
            "a finished playlist stays put"
        );
    }

    #[test]
    fn the_target_duration_is_the_configured_one_whatever_the_window_holds() {
        let mut window = window();
        let empty = window.snapshot();

        window.push(segment(0, false));
        let running = window.snapshot();
        window.push(Segment {
            duration_ts: 6 * Consts::TIMESCALE,
            ..segment(1, false)
        });
        let overlong = window.snapshot();

        for playlist in [&empty, &running, &overlong] {
            assert!(
                playlist.playlist.contains("#EXT-X-TARGETDURATION:4\n"),
                "a client is told one target duration for the life of the stream: {}",
                playlist.playlist
            );
        }
    }

    #[test]
    fn a_window_of_short_segments_keeps_the_configured_target_duration() {
        let mut window = window();

        window.push(Segment {
            duration_ts: Consts::TIMESCALE / 2,
            ..segment(0, true)
        });

        assert!(
            window
                .snapshot()
                .playlist
                .contains("#EXT-X-TARGETDURATION:4\n"),
            "a drop-truncated segment must not lower what the client was told: {}",
            window.snapshot().playlist
        );
    }

    #[test]
    fn an_empty_window_renders_a_playlist_with_no_segments() {
        assert_eq!(
            window().snapshot().playlist.as_ref(),
            "#EXTM3U\n\
             #EXT-X-VERSION:3\n\
             #EXT-X-TARGETDURATION:4\n\
             #EXT-X-MEDIA-SEQUENCE:0\n\
             #EXT-X-DISCONTINUITY-SEQUENCE:0\n"
        );
    }

    #[test]
    fn the_rendered_playlist_is_grammatical_hls() {
        let mut window = window();
        fill(&mut window, 10);
        window.push(segment(10, true));

        let live = window.snapshot();
        let parsed = hls_m3u8::MediaPlaylist::try_from(live.playlist.as_ref())
            .expect("the live playlist parses");
        assert_eq!(parsed.media_sequence, 5);
        assert_eq!(parsed.discontinuity_sequence, 0);
        assert_eq!(parsed.segments.iter().count(), BroadcastConfig::WINDOW);
        assert!(!parsed.has_end_list);

        window.finish();
        let finished = window.snapshot();
        let parsed = hls_m3u8::MediaPlaylist::try_from(finished.playlist.as_ref())
            .expect("the finished playlist parses");
        assert!(parsed.has_end_list);
    }

    #[test]
    fn a_window_the_playlist_rules_reject_is_refused() {
        assert!(LiveWindow::new(&BroadcastConfig::builder().window(0).build()).is_err());
        assert!(LiveWindow::new(&BroadcastConfig::builder().sample_rate(0).build()).is_err());
        assert!(
            LiveWindow::new(
                &BroadcastConfig::builder()
                    .segment_target(Duration::from_millis(500))
                    .window(5)
                    .build()
            )
            .is_err()
        );
    }
}
