use std::net::TcpStream;

use kithara::{
    self,
    platform::{thread, time::Duration},
};

use super::origin::{
    GRACE, Origin, SAMPLE_RATE, SEGMENT_FRAMES, TONE_HZ, WINDOW, assert_carries_the_tone,
    decode_adts_left,
};

/// Frames the AAC-LC encoder needs before the tone is fully formed.
const PRIMING_SKIP_FRAMES: usize = 4_800;
const NOT_FOUND: u16 = 404;
/// Polls allowed while the cancelled origin finishes closing its socket.
const CONNECT_ATTEMPTS: usize = 200;

fn assert_tone(pcm: &[f32], label: &str) {
    assert_carries_the_tone(pcm, TONE_HZ, SAMPLE_RATE, label);
}

fn listed_sequences(playlist: &str) -> Vec<u64> {
    playlist
        .lines()
        .filter_map(|line| line.strip_prefix("seg/"))
        .filter_map(|line| line.strip_suffix(".aac"))
        .map(|seq| seq.parse().expect("a segment sequence number"))
        .collect()
}

fn tag_value(playlist: &str, tag: &str) -> u64 {
    playlist
        .lines()
        .find_map(|line| line.strip_prefix(tag))
        .unwrap_or_else(|| panic!("{tag} is missing from {playlist}"))
        .parse()
        .expect("a numeric tag value")
}

#[kithara::test(tokio)]
async fn the_media_playlist_arrives_only_once_a_segment_exists() {
    let origin = Origin::start();

    assert_eq!(
        origin.get("v/0/live.m3u8").await.err(),
        Some(NOT_FOUND),
        "a playlist with no segments is not a playlist a client can start on"
    );
    assert!(
        origin.get("master.m3u8").await.is_ok(),
        "the master playlist stands before the first segment"
    );

    origin.advance_to(1);

    assert!(origin.get("v/0/live.m3u8").await.is_ok());
}

#[kithara::test(tokio)]
async fn the_live_playlist_slides_a_bounded_window() {
    let origin = Origin::start();
    origin.advance_to(3);

    let early = origin.media_playlist().await;
    let target = tag_value(&early, "#EXT-X-TARGETDURATION:");
    assert!(!early.contains("#EXT-X-ENDLIST"), "the stream is live");
    assert_eq!(tag_value(&early, "#EXT-X-MEDIA-SEQUENCE:"), 0);
    assert_eq!(listed_sequences(&early).len(), 3);

    origin.advance_to(u64::try_from(WINDOW).expect("fits") + 3);
    let late = origin.media_playlist().await;

    assert!(!late.contains("#EXT-X-ENDLIST"));
    assert_eq!(
        listed_sequences(&late).len(),
        WINDOW,
        "the window is bounded"
    );
    assert!(
        tag_value(&late, "#EXT-X-MEDIA-SEQUENCE:") > 0,
        "the media sequence advances once the window is full: {late}"
    );
    assert_eq!(
        tag_value(&late, "#EXT-X-TARGETDURATION:"),
        target,
        "the target duration a client was told cannot change"
    );

    let master = origin.get("master.m3u8").await.expect("a master playlist");
    let master = String::from_utf8(master.to_vec()).expect("the master playlist is text");
    let variant = master
        .lines()
        .find(|line| line.ends_with(".m3u8") && !line.starts_with('#'))
        .expect("the master lists a variant");
    assert!(
        origin.get(variant).await.is_ok(),
        "the master's variant URI leads to the media playlist"
    );
}

#[kithara::test(tokio)]
async fn an_evicted_segment_outlives_the_playlist_by_the_grace() {
    let origin = Origin::start();
    origin.advance_to(u64::try_from(WINDOW + GRACE).expect("fits") + 1);

    let playlist = origin.media_playlist().await;
    let listed = listed_sequences(&playlist);
    let first_listed = listed.first().copied().expect("a listed segment");

    assert!(first_listed > 0, "the window has slid: {playlist}");
    assert!(
        origin
            .get(&format!("v/0/seg/{}.aac", first_listed - 1))
            .await
            .is_ok(),
        "the segment just off the playlist is still fetchable"
    );
    assert_eq!(
        origin.get("v/0/seg/0.aac").await.err(),
        Some(NOT_FOUND),
        "the segment past the retention is gone"
    );
}

#[kithara::test(tokio)]
async fn the_fetched_segments_decode_back_to_the_source_tone() {
    let origin = Origin::start();
    origin.advance_to(4);

    let playlist = origin.media_playlist().await;
    let mut stream = Vec::new();
    for seq in listed_sequences(&playlist) {
        let bytes = origin
            .get(&format!("v/0/seg/{seq}.aac"))
            .await
            .expect("a listed segment is fetchable");
        stream.extend_from_slice(&bytes);
    }

    let decoded = decode_adts_left(stream);
    let expected = usize::try_from(SEGMENT_FRAMES * 4).expect("fits");
    assert!(
        decoded.len() >= expected / 2,
        "expected about {expected} frames of audio, decoded {}",
        decoded.len()
    );
    assert_tone(&decoded[PRIMING_SKIP_FRAMES..], "fetched stream");
}

#[kithara::test(tokio)]
async fn stopping_leaves_a_fetchable_vod_tail() {
    let origin = Origin::start();
    origin.advance_to(3);
    origin.handle.stop();

    let playlist = origin.media_playlist().await;
    assert!(
        playlist.contains("#EXT-X-ENDLIST\n"),
        "a stopped stream is a VOD playlist: {playlist}"
    );
    assert!(!origin.handle.status().is_live);

    let listed = listed_sequences(&playlist);
    let joined = listed
        .iter()
        .rev()
        .nth(1)
        .copied()
        .expect("a full segment before the tail");
    let bytes = origin
        .get(&format!("v/0/seg/{joined}.aac"))
        .await
        .expect("the tail is fetchable");

    let decoded = decode_adts_left(bytes.to_vec());
    assert_tone(
        &decoded[PRIMING_SKIP_FRAMES..],
        &format!("segment {joined}"),
    );
}

#[kithara::test(tokio)]
async fn cancelling_the_parent_stops_the_origin_and_the_worker() {
    let origin = Origin::start();
    origin.advance_to(2);
    let addr = origin
        .handle
        .url()
        .trim_start_matches("http://")
        .trim_end_matches("/master.m3u8")
        .to_owned();

    origin.shutdown();
    origin.handle.stop();

    for attempt in 0..CONNECT_ATTEMPTS {
        if TcpStream::connect(&addr).is_err() {
            return;
        }
        assert!(
            attempt + 1 < CONNECT_ATTEMPTS,
            "the origin still accepts connections after its token was cancelled"
        );
        thread::paced_backoff(Duration::from_millis(5));
    }
}
