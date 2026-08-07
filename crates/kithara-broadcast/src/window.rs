use std::collections::VecDeque;

use bytes::Bytes;
use kithara_platform::sync::Arc;

use crate::{BroadcastError, BroadcastResult, segment::Segment};

/// Value view of the live stream: rendered playlist plus the segments a client
/// can still fetch. Cloning shares both.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct PlaylistSnapshot {
    pub playlist: Arc<str>,
    pub segments: Arc<[Segment]>,
    pub finished: bool,
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
    segments: VecDeque<Segment>,
    finished: bool,
}

impl LiveWindow {
    /// Segments a client sees in the playlist.
    pub const WINDOW: usize = 6;

    /// Segments kept fetchable past the playlist window.
    pub const GRACE: usize = 3;

    /// Open a window of `window` playlist segments with `grace` extra retained
    /// segments, over access units timed in `timescale` units.
    ///
    /// # Errors
    ///
    /// Returns [`BroadcastError::InvalidConfig`] for a zero window or timescale.
    pub fn new(window: usize, grace: usize, timescale: u32) -> BroadcastResult<Self> {
        if window == 0 {
            return Err(BroadcastError::InvalidConfig { field: "window" });
        }
        if timescale == 0 {
            return Err(BroadcastError::InvalidConfig { field: "timescale" });
        }

        Ok(Self {
            window,
            retention: window + grace,
            timescale,
            segments: VecDeque::with_capacity(window + grace),
            finished: false,
        })
    }

    /// Append a closed segment, evicting whatever falls past the retention.
    pub fn push(&mut self, segment: Segment) {
        self.segments.push_back(segment);
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
            finished: self.finished,
        }
    }

    fn render(&self) -> String {
        let listed = self.segments.len().saturating_sub(self.window);
        let listed = self.segments.range(listed..);
        let target = listed
            .clone()
            .map(|segment| u64::from(segment.duration_ts))
            .max()
            .unwrap_or_default()
            .div_ceil(u64::from(self.timescale));
        let media_sequence = listed.clone().next().map_or(0, |segment| segment.seq);

        let mut playlist = format!(
            "#EXTM3U\n\
             #EXT-X-VERSION:3\n\
             #EXT-X-TARGETDURATION:{target}\n\
             #EXT-X-MEDIA-SEQUENCE:{media_sequence}\n"
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

    use super::LiveWindow;
    use crate::segment::Segment;

    struct Consts;

    impl Consts {
        const DURATION_TS: u32 = 192_512;
        const TIMESCALE: u32 = 48_000;
    }

    fn window() -> LiveWindow {
        LiveWindow::new(LiveWindow::WINDOW, LiveWindow::GRACE, Consts::TIMESCALE).expect("window")
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
        for seq in 0..count {
            window.push(segment(seq, false));
        }
    }

    #[test]
    fn the_playlist_holds_the_last_window_of_segments() {
        let mut window = window();

        fill(&mut window, 10);

        assert_eq!(
            window.snapshot().playlist.as_ref(),
            "#EXTM3U\n\
             #EXT-X-VERSION:3\n\
             #EXT-X-TARGETDURATION:5\n\
             #EXT-X-MEDIA-SEQUENCE:4\n\
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
             #EXT-X-TARGETDURATION:5\n\
             #EXT-X-MEDIA-SEQUENCE:0\n\
             #EXTINF:4.011,\n\
             seg/0.aac\n\
             #EXT-X-DISCONTINUITY\n\
             #EXTINF:4.011,\n\
             seg/1.aac\n"
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
        assert!(!live.finished);
        assert_eq!(
            finished.playlist.as_ref(),
            format!("{}#EXT-X-ENDLIST\n", live.playlist)
        );
        assert!(finished.finished);
        assert_eq!(
            window.snapshot().playlist,
            finished.playlist,
            "a finished playlist stays put"
        );
    }

    #[test]
    fn the_target_duration_rounds_up_the_longest_segment() {
        let mut window = window();

        window.push(Segment {
            duration_ts: 3 * Consts::TIMESCALE,
            ..segment(0, false)
        });
        window.push(Segment {
            duration_ts: 6 * Consts::TIMESCALE + 1,
            ..segment(1, false)
        });

        assert!(
            window
                .snapshot()
                .playlist
                .contains("#EXT-X-TARGETDURATION:7\n"),
            "{}",
            window.snapshot().playlist
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
        assert_eq!(parsed.segments.iter().count(), LiveWindow::WINDOW);
        assert!(!parsed.has_end_list);

        window.finish();
        let finished = window.snapshot();
        let parsed = hls_m3u8::MediaPlaylist::try_from(finished.playlist.as_ref())
            .expect("the finished playlist parses");
        assert!(parsed.has_end_list);
    }

    #[test]
    fn a_zero_window_or_timescale_is_rejected() {
        assert!(LiveWindow::new(0, LiveWindow::GRACE, Consts::TIMESCALE).is_err());
        assert!(LiveWindow::new(LiveWindow::WINDOW, LiveWindow::GRACE, 0).is_err());
    }
}
