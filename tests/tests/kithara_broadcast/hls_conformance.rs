use kithara_integration_tests::packed_audio::{PackedSegment, TIMESTAMP_BITS};

use super::origin::{Origin, SAMPLE_RATE, WINDOW};

const POLLS: usize = 4;
const SEGMENTS_PER_POLL: u64 = 2;
const MIN_TARGETS: f64 = 3.0;
const AU_SLACK: f64 = 1.0;
const SAMPLES_PER_AU: f64 = 1_024.0;
const TIMESTAMP_SLACK: u64 = 1;

#[derive(Debug, Clone, PartialEq)]
struct Entry {
    extinf: String,
    uri: String,
    seconds: f64,
}

#[derive(Debug, Clone)]
struct Playlist {
    text: String,
    target: f64,
    target_text: String,
    media_sequence: u64,
    discontinuity_sequence: u64,
    entries: Vec<Entry>,
}

impl Playlist {
    fn parse(text: String) -> Self {
        let mut entries = Vec::new();
        let mut extinf: Option<(String, f64)> = None;
        for line in text.lines() {
            if let Some(value) = line.strip_prefix("#EXTINF:") {
                let seconds = value
                    .trim_end_matches(',')
                    .parse()
                    .expect("EXTINF carries a duration");
                extinf = Some((line.to_owned(), seconds));
            } else if !line.starts_with('#') && !line.is_empty() {
                let (extinf, seconds) = extinf.take().expect("a segment URI follows its EXTINF");
                entries.push(Entry {
                    extinf,
                    uri: line.to_owned(),
                    seconds,
                });
            }
        }

        let target_text = tag(&text, "#EXT-X-TARGETDURATION:").to_owned();
        Self {
            target: target_text.parse().expect("a numeric target duration"),
            target_text,
            media_sequence: tag(&text, "#EXT-X-MEDIA-SEQUENCE:")
                .parse()
                .expect("a numeric media sequence"),
            discontinuity_sequence: tag(&text, "#EXT-X-DISCONTINUITY-SEQUENCE:")
                .parse()
                .expect("a numeric discontinuity sequence"),
            entries,
            text,
        }
    }

    fn spans(&self) -> f64 {
        self.entries.iter().map(|entry| entry.seconds).sum()
    }
}

fn tag<'a>(text: &'a str, tag: &str) -> &'a str {
    text.lines()
        .find_map(|line| line.strip_prefix(tag))
        .unwrap_or_else(|| panic!("{tag} is missing from {text}"))
}

#[kithara::test(tokio)]
async fn the_live_playlist_obeys_the_reload_rules() {
    let origin = Origin::start();
    let listed = u64::try_from(WINDOW).expect("the window fits");

    let mut polls = Vec::with_capacity(POLLS);
    for poll in 0..POLLS {
        origin.advance_to(listed + SEGMENTS_PER_POLL * u64::try_from(poll).expect("fits"));
        polls.push(Playlist::parse(origin.media_playlist().await));
    }

    for playlist in &polls {
        assert_eq!(
            playlist.target_text, polls[0].target_text,
            "the target duration must not change between reloads: {}",
            playlist.text
        );
        assert!(
            playlist.spans() >= MIN_TARGETS * playlist.target,
            "a live playlist spans at least three target durations: {}",
            playlist.text
        );
        for entry in &playlist.entries {
            assert!(
                entry.seconds.round() <= playlist.target,
                "{} rounds past the {} s target duration",
                entry.extinf,
                playlist.target
            );
        }
    }

    for pair in polls.windows(2) {
        let (before, after) = (&pair[0], &pair[1]);
        assert!(
            after.media_sequence >= before.media_sequence,
            "the media sequence never goes back"
        );
        assert!(
            after.discontinuity_sequence >= before.discontinuity_sequence,
            "the discontinuity sequence never goes back"
        );

        let gone = usize::try_from(after.media_sequence - before.media_sequence).expect("fits");
        assert!(
            gone <= before.entries.len(),
            "the playlist advanced past segments a client never saw"
        );
        let overlap = &before.entries[gone..];
        assert_eq!(
            overlap,
            &after.entries[..overlap.len()],
            "the segments both reloads list must be identical, and new ones append"
        );
    }
    let last = polls.last().expect("the polls were collected");
    assert!(
        last.media_sequence > polls[0].media_sequence,
        "the window slid across the polls"
    );
}

#[kithara::test(tokio)]
async fn a_gap_in_the_feed_is_signalled_for_a_client_to_resynchronise() {
    let origin = Origin::start();
    let listed = u64::try_from(WINDOW).expect("the window fits");
    origin.advance_to(2);
    let before = Playlist::parse(origin.media_playlist().await);

    origin.drop_samples(u64::from(SAMPLE_RATE));
    origin.advance_to(5);
    let marked = Playlist::parse(origin.media_playlist().await);

    assert!(
        marked.text.contains("#EXT-X-DISCONTINUITY\n"),
        "the segment after the gap is discontinuous: {}",
        marked.text
    );
    assert_eq!(
        marked.target_text, before.target_text,
        "the segment the gap cut short must not lower the announced target duration: {}",
        marked.text
    );
    assert_eq!(
        marked.discontinuity_sequence, before.discontinuity_sequence,
        "a listed discontinuity is still the client's to see"
    );

    origin.advance_to(listed + 4);
    let slid = Playlist::parse(origin.media_playlist().await);

    assert!(
        !slid.text.contains("#EXT-X-DISCONTINUITY\n"),
        "the discontinuity has left the playlist: {}",
        slid.text
    );
    assert_eq!(
        slid.discontinuity_sequence,
        before.discontinuity_sequence + 1,
        "a client reloading here can still place the discontinuity: {}",
        slid.text
    );
}

#[kithara::test(tokio)]
async fn every_segment_is_a_packed_audio_segment() {
    let origin = Origin::start();
    origin.advance_to(u64::try_from(WINDOW).expect("fits"));

    let playlist = Playlist::parse(origin.media_playlist().await);
    let mut segments = Vec::new();
    for entry in &playlist.entries {
        let bytes = origin
            .get(&format!("v/0/{}", entry.uri))
            .await
            .expect("a listed segment is fetchable");
        let parsed = PackedSegment::parse(&bytes)
            .unwrap_or_else(|error| panic!("{} is not packed audio: {error}", entry.uri));
        segments.push((entry.clone(), parsed));
    }

    let first = segments.first().expect("a segment").1.frames[0];
    for (entry, segment) in &segments {
        assert_eq!(
            segment.timestamp >> TIMESTAMP_BITS,
            0,
            "{}: the section 3.4 timestamp uses 33 bits",
            entry.uri
        );
        assert_eq!(
            &segment.timestamp_bytes[..3],
            &[0, 0, 0],
            "{}: the top 31 bits of the timestamp are zero",
            entry.uri
        );

        for frame in &segment.frames {
            assert_eq!(
                (frame.profile, frame.sample_rate, frame.channels),
                (first.profile, first.sample_rate, first.channels),
                "{}: every ADTS frame describes the same audio",
                entry.uri
            );
        }
        assert!(
            (segment.frame_seconds() - entry.seconds).abs()
                <= AU_SLACK * SAMPLES_PER_AU / f64::from(first.sample_rate),
            "{}: {} frames carry {:.3} s against the declared {:.3} s",
            entry.uri,
            segment.frames.len(),
            segment.frame_seconds(),
            entry.seconds
        );
    }

    for pair in segments.windows(2) {
        let expected = pair[0].1.next_timestamp();
        assert!(
            pair[1].1.timestamp.abs_diff(expected) <= TIMESTAMP_SLACK,
            "{} opens at {} where {} ends at {expected}",
            pair[1].0.uri,
            pair[1].1.timestamp,
            pair[0].0.uri
        );
    }
}

#[kithara::test(tokio)]
async fn a_stopped_playlist_is_frozen() {
    let origin = Origin::start();
    origin.advance_to(3);
    origin.handle.stop();

    let first = origin.media_playlist().await;
    let second = origin.media_playlist().await;

    assert!(first.contains("#EXT-X-ENDLIST\n"));
    assert_eq!(first, second, "a finished playlist never changes again");
}

#[kithara::test(tokio)]
async fn the_master_playlist_declares_the_stream() {
    let origin = Origin::start();

    let master = origin.get("master.m3u8").await.expect("a master playlist");
    let master = String::from_utf8(master.to_vec()).expect("the master playlist is text");
    let variant = master
        .lines()
        .find_map(|line| line.strip_prefix("#EXT-X-STREAM-INF:"))
        .expect("the master declares one variant");

    let attributes: Vec<&str> = variant.split(',').collect();
    let bandwidth = attributes
        .iter()
        .find_map(|attribute| attribute.strip_prefix("BANDWIDTH="))
        .expect("the variant declares a bandwidth");
    assert!(
        bandwidth.parse::<u64>().expect("a numeric bandwidth") > 0,
        "BANDWIDTH must be a positive bit rate"
    );
    assert!(
        attributes.contains(&"CODECS=\"mp4a.40.2\""),
        "the variant declares AAC-LC: {variant}"
    );
}
