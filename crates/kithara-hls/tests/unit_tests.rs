#![forbid(unsafe_code)]

use kithara_hls::{
    HlsError,
    playlist::{VariantId, parse_master_playlist, parse_media_playlist},
};
use rstest::{fixture, rstest};

// ==================== Fixtures ====================

#[fixture]
fn variant_id_42() -> VariantId {
    VariantId(42)
}

#[fixture]
fn variant_id_5() -> VariantId {
    VariantId(5)
}

#[fixture]
fn variant_id_0() -> VariantId {
    VariantId(0)
}

#[fixture]
fn simple_master_playlist_data() -> &'static [u8] {
    b"#EXTM3U
#EXT-X-VERSION:6
#EXT-X-STREAM-INF:BANDWIDTH=1000000,CODECS=\"mp4a.40.2\"
audio.m3u8
#EXT-X-STREAM-INF:BANDWIDTH=2000000,CODECS=\"mp4a.40.2\"
audio_high.m3u8"
}

#[fixture]
fn simple_media_playlist_data() -> &'static [u8] {
    b"#EXTM3U
#EXT-X-VERSION:6
#EXT-X-TARGETDURATION:4
#EXT-X-MEDIA-SEQUENCE:0
#EXT-X-PLAYLIST-TYPE:VOD
#EXTINF:4.0,
segment0.ts
#EXTINF:4.0,
segment1.ts
#EXT-X-ENDLIST"
}

#[fixture]
fn media_playlist_with_init_data() -> &'static [u8] {
    b"#EXTM3U
#EXT-X-VERSION:6
#EXT-X-TARGETDURATION:4
#EXT-X-MEDIA-SEQUENCE:0
#EXT-X-MAP:URI=\"init.mp4\"
#EXTINF:4.0,
segment0.m4s
#EXT-X-ENDLIST"
}

#[fixture]
fn invalid_playlist_data() -> &'static [u8] {
    b"NOT A VALID PLAYLIST"
}

#[fixture]
fn empty_master_playlist_data() -> &'static [u8] {
    b"#EXTM3U
#EXT-X-VERSION:6"
}

#[fixture]
fn master_playlist_with_codec_data() -> &'static [u8] {
    b"#EXTM3U
#EXT-X-VERSION:6
#EXT-X-STREAM-INF:BANDWIDTH=1000000,CODECS=\"mp4a.40.2,avc1.64001f\",RESOLUTION=1280x720
video.m3u8"
}

// ==================== Test Cases ====================

#[rstest]
fn test_variant_id_creation(variant_id_42: VariantId) {
    assert_eq!(variant_id_42.0, 42);
}

#[rstest]
fn test_parse_simple_master_playlist(simple_master_playlist_data: &[u8]) {
    let result = parse_master_playlist(simple_master_playlist_data);
    assert!(
        result.is_ok(),
        "Failed to parse master playlist: {:?}",
        result.err()
    );

    let master = result.unwrap();
    assert_eq!(master.variants.len(), 2);

    assert_eq!(master.variants[0].id.0, 0);
    assert_eq!(master.variants[0].uri, "audio.m3u8");
    assert_eq!(master.variants[0].bandwidth, Some(1000000));

    assert_eq!(master.variants[1].id.0, 1);
    assert_eq!(master.variants[1].uri, "audio_high.m3u8");
    assert_eq!(master.variants[1].bandwidth, Some(2000000));
}

#[rstest]
fn test_parse_simple_media_playlist(simple_media_playlist_data: &[u8], variant_id_0: VariantId) {
    let result = parse_media_playlist(simple_media_playlist_data, variant_id_0);
    assert!(
        result.is_ok(),
        "Failed to parse media playlist: {:?}",
        result.err()
    );

    let media = result.unwrap();
    assert_eq!(media.segments.len(), 2);
    assert_eq!(
        media.target_duration,
        Some(std::time::Duration::from_secs_f64(4.0))
    );
    assert_eq!(media.media_sequence, 0);
    assert!(media.end_list);

    assert_eq!(media.segments[0].variant_id.0, 0);
    assert_eq!(media.segments[0].uri, "segment0.ts");
    assert_eq!(media.segments[0].sequence, 0);

    assert_eq!(media.segments[1].variant_id.0, 0);
    assert_eq!(media.segments[1].uri, "segment1.ts");
    assert_eq!(media.segments[1].sequence, 1);
}

#[rstest]
fn test_parse_media_playlist_with_init_segment(
    media_playlist_with_init_data: &[u8],
    variant_id_0: VariantId,
) {
    let result = parse_media_playlist(media_playlist_with_init_data, variant_id_0);
    assert!(
        result.is_ok(),
        "Failed to parse media playlist with init: {:?}",
        result.err()
    );

    let media = result.unwrap();
    assert_eq!(media.segments.len(), 1);
    assert!(media.init_segment.is_some());

    let init = media.init_segment.unwrap();
    assert_eq!(init.uri, "init.mp4");
}

#[rstest]
fn test_parse_invalid_playlist(invalid_playlist_data: &[u8]) {
    let result = parse_master_playlist(invalid_playlist_data);
    assert!(result.is_err(), "Should fail to parse invalid playlist");

    if let Err(HlsError::PlaylistParse(_)) = result {
        // Expected error type
    } else {
        panic!("Expected PlaylistParse error, got: {:?}", result);
    }
}

#[rstest]
fn test_empty_master_playlist(empty_master_playlist_data: &[u8]) {
    let result = parse_master_playlist(empty_master_playlist_data);
    assert!(result.is_ok(), "Empty master playlist should parse");

    let master = result.unwrap();
    assert_eq!(master.variants.len(), 0);
}

#[rstest]
fn test_media_playlist_variant_id_preserved(
    simple_media_playlist_data: &[u8],
    variant_id_5: VariantId,
) {
    let result = parse_media_playlist(simple_media_playlist_data, variant_id_5);
    assert!(result.is_ok());

    let media = result.unwrap();
    assert_eq!(media.segments.len(), 2);
    assert_eq!(media.segments[0].variant_id.0, 5);
    assert_eq!(media.segments[1].variant_id.0, 5);
}

#[rstest]
fn test_master_playlist_with_codec_info(master_playlist_with_codec_data: &[u8]) {
    let result = parse_master_playlist(master_playlist_with_codec_data);
    assert!(result.is_ok());

    let master = result.unwrap();
    assert_eq!(master.variants.len(), 1);

    let variant = &master.variants[0];
    assert!(variant.codec.is_some());

    let codec = variant.codec.as_ref().unwrap();
    let codecs_str = codec.codecs.as_deref().unwrap_or("");
    assert!(codecs_str.contains("mp4a.40.2"));
    assert!(codecs_str.contains("avc1.64001f"));
}

#[rstest]
#[case(HlsError::PlaylistParse("test error".to_string()), "test error")]
#[case(HlsError::VariantNotFound("variant 0".to_string()), "variant 0")]
#[case(HlsError::InvalidUrl("bad url".to_string()), "bad url")]
fn test_error_types(#[case] error: HlsError, #[case] expected_substring: &str) {
    let error_str = error.to_string();
    assert!(
        error_str.contains(expected_substring),
        "Error string '{}' should contain '{}'",
        error_str,
        expected_substring
    );
}

#[rstest]
#[case(0u32, "test.m3u8", Some(1000000), Some("Test Variant".to_string()))]
#[case(1u32, "audio.m3u8", Some(2000000), None)]
#[case(2u32, "video.m3u8", None, Some("Video Only".to_string()))]
fn test_playlist_struct_debug_variant(
    #[case] id: u32,
    #[case] uri: &str,
    #[case] bandwidth: Option<u64>,
    #[case] name: Option<String>,
) {
    let variant = kithara_hls::playlist::VariantStream {
        id: VariantId(id as usize),
        uri: uri.to_string(),
        bandwidth,
        name: name.clone(),
        codec: None,
    };

    let debug_output = format!("{:?}", variant);
    assert!(debug_output.contains("VariantStream"));
    assert!(debug_output.contains(uri));

    if let Some(name_str) = name {
        assert!(debug_output.contains(&name_str));
    }
}

#[rstest]
#[case(0u32, 0u64, "segment.ts", 4.0)]
#[case(5u32, 1u64, "segment1.m4s", 6.0)]
#[case(10u32, 2u64, "chunk.ts", 2.5)]
fn test_playlist_struct_debug_segment(
    #[case] variant_id: u32,
    #[case] sequence: u64,
    #[case] uri: &str,
    #[case] duration_secs: f64,
) {
    let segment = kithara_hls::playlist::MediaSegment {
        sequence,
        variant_id: VariantId(variant_id as usize),
        uri: uri.to_string(),
        duration: std::time::Duration::from_secs_f64(duration_secs),
        key: None,
    };

    let debug_output = format!("{:?}", segment);
    assert!(debug_output.contains("MediaSegment"));
    assert!(debug_output.contains(uri));
}

#[rstest]
#[case(
    b"#EXTM3U\n#EXT-X-VERSION:6\n#EXT-X-STREAM-INF:BANDWIDTH=500000\nlow.m3u8",
    1
)]
#[case(b"#EXTM3U\n#EXT-X-VERSION:6\n#EXT-X-STREAM-INF:BANDWIDTH=1000000\nmid.m3u8\n#EXT-X-STREAM-INF:BANDWIDTH=2000000\nhigh.m3u8", 2)]
#[case(b"#EXTM3U\n#EXT-X-VERSION:6", 0)]
fn test_master_playlist_variant_count(#[case] data: &[u8], #[case] expected_count: usize) {
    let result = parse_master_playlist(data);
    assert!(
        result.is_ok(),
        "Failed to parse playlist: {:?}",
        result.err()
    );

    let master = result.unwrap();
    assert_eq!(master.variants.len(), expected_count);
}

#[rstest]
#[case(
    b"#EXTM3U\n#EXT-X-VERSION:6\n#EXT-X-TARGETDURATION:4\n#EXTINF:4.0,\nseg1.ts\n#EXT-X-ENDLIST",
    1,
    true
)]
#[case(b"#EXTM3U\n#EXT-X-VERSION:6\n#EXT-X-TARGETDURATION:4\n#EXTINF:4.0,\nseg1.ts\n#EXTINF:4.0,\nseg2.ts", 2, false)]
#[case(b"#EXTM3U\n#EXT-X-VERSION:6\n#EXT-X-TARGETDURATION:4\n#EXTINF:4.0,\nseg1.ts\n#EXTINF:4.0,\nseg2.ts\n#EXTINF:4.0,\nseg3.ts\n#EXT-X-ENDLIST", 3, true)]
fn test_media_playlist_segment_count_and_endlist(
    #[case] data: &[u8],
    #[case] expected_segments: usize,
    #[case] expected_endlist: bool,
    variant_id_0: VariantId,
) {
    let result = parse_media_playlist(data, variant_id_0);
    assert!(
        result.is_ok(),
        "Failed to parse playlist: {:?}",
        result.err()
    );

    let media = result.unwrap();
    assert_eq!(media.segments.len(), expected_segments);
    assert_eq!(media.end_list, expected_endlist);
}
