use std::net::TcpStream;

use kithara::{
    self,
    platform::{thread, time::Duration},
};

use super::origin::{
    GRACE, Origin, Playlist, SAMPLE_RATE, SEGMENT_FRAMES, TONE_HZ, WINDOW, assert_carries_the_tone,
    decode_adts_left,
};

const PRIMING_SKIP_FRAMES: usize = 4_800;
const NOT_FOUND: u16 = 404;
const CONNECT_ATTEMPTS: usize = 200;

fn assert_tone(pcm: &[f32], label: &str) {
    assert_carries_the_tone(pcm, TONE_HZ, SAMPLE_RATE, label);
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

    let early = Playlist::parse(origin.media_playlist().await);
    let target = early.target;
    assert!(!early.text.contains("#EXT-X-ENDLIST"), "the stream is live");
    assert_eq!(early.media_sequence, 0);
    assert_eq!(early.entries.len(), 3);

    origin.advance_to(u64::try_from(WINDOW).expect("fits") + 3);
    let late = Playlist::parse(origin.media_playlist().await);

    assert!(!late.text.contains("#EXT-X-ENDLIST"));
    assert_eq!(late.entries.len(), WINDOW, "the window is bounded");
    assert!(
        late.media_sequence > 0,
        "the media sequence advances once the window is full: {}",
        late.text
    );
    assert_eq!(
        late.target, target,
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

    let playlist = Playlist::parse(origin.media_playlist().await);
    let listed = playlist.sequences();
    let first_listed = listed.first().copied().expect("a listed segment");

    assert!(first_listed > 0, "the window has slid: {}", playlist.text);
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

    let playlist = Playlist::parse(origin.media_playlist().await);
    let mut stream = Vec::new();
    for seq in playlist.sequences() {
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

    let playlist = Playlist::parse(origin.media_playlist().await);
    assert!(
        playlist.text.contains("#EXT-X-ENDLIST\n"),
        "a stopped stream is a VOD playlist: {}",
        playlist.text
    );
    assert!(!origin.handle.status().is_live);

    let listed = playlist.sequences();
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

#[kithara::test(tokio, timeout(Duration::from_secs(20)))]
async fn a_live_origin_leaves_the_virtual_clock_free() {
    const A_DAY: Duration = Duration::from_secs(86_400);

    let origin = Origin::start();
    origin.advance_to(1);

    kithara::platform::time::sleep(A_DAY).await;

    assert!(origin.handle.status().is_live);
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
