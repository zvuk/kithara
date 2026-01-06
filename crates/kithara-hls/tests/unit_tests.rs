#![forbid(unsafe_code)]

use kithara_hls::{
    HlsError,
    playlist::{VariantId, parse_master_playlist, parse_media_playlist},
};

#[test]
fn test_variant_id_creation() {
    let id = VariantId(42);
    assert_eq!(id.0, 42);
}

#[test]
fn test_parse_simple_master_playlist() {
    let data = b"#EXTM3U
#EXT-X-VERSION:6
#EXT-X-STREAM-INF:BANDWIDTH=1000000,CODECS=\"mp4a.40.2\"
audio.m3u8
#EXT-X-STREAM-INF:BANDWIDTH=2000000,CODECS=\"mp4a.40.2\"
audio_high.m3u8";

    let result = parse_master_playlist(data);
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

#[test]
fn test_parse_simple_media_playlist() {
    let data = b"#EXTM3U
#EXT-X-VERSION:6
#EXT-X-TARGETDURATION:4
#EXT-X-MEDIA-SEQUENCE:0
#EXT-X-PLAYLIST-TYPE:VOD
#EXTINF:4.0,
segment0.ts
#EXTINF:4.0,
segment1.ts
#EXT-X-ENDLIST";

    let result = parse_media_playlist(data, VariantId(0));
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

#[test]
fn test_parse_media_playlist_with_init_segment() {
    let data = b"#EXTM3U
#EXT-X-VERSION:6
#EXT-X-TARGETDURATION:4
#EXT-X-MEDIA-SEQUENCE:0
#EXT-X-MAP:URI=\"init.mp4\"
#EXTINF:4.0,
segment0.m4s
#EXT-X-ENDLIST";

    let result = parse_media_playlist(data, VariantId(0));
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

#[test]
fn test_parse_invalid_playlist() {
    let data = b"NOT A VALID PLAYLIST";
    let result = parse_master_playlist(data);
    assert!(result.is_err(), "Should fail to parse invalid playlist");

    if let Err(HlsError::PlaylistParse(_)) = result {
        // Expected error type
    } else {
        panic!("Expected PlaylistParse error, got: {:?}", result);
    }
}

#[test]
fn test_empty_master_playlist() {
    let data = b"#EXTM3U
#EXT-X-VERSION:6";

    let result = parse_master_playlist(data);
    assert!(result.is_ok(), "Empty master playlist should parse");

    let master = result.unwrap();
    assert_eq!(master.variants.len(), 0);
}

#[test]
fn test_media_playlist_variant_id_preserved() {
    let data = b"#EXTM3U
#EXT-X-VERSION:6
#EXT-X-TARGETDURATION:4
#EXTINF:4.0,
segment.ts
#EXT-X-ENDLIST";

    let variant_id = VariantId(5);
    let result = parse_media_playlist(data, variant_id);
    assert!(result.is_ok());

    let media = result.unwrap();
    assert_eq!(media.segments.len(), 1);
    assert_eq!(media.segments[0].variant_id.0, 5);
}

#[test]
fn test_master_playlist_with_codec_info() {
    let data = b"#EXTM3U
#EXT-X-VERSION:6
#EXT-X-STREAM-INF:BANDWIDTH=1000000,CODECS=\"mp4a.40.2,avc1.64001f\",RESOLUTION=1280x720
video.m3u8";

    let result = parse_master_playlist(data);
    assert!(result.is_ok());

    let master = result.unwrap();
    assert_eq!(master.variants.len(), 1);

    let variant = &master.variants[0];
    assert!(variant.codec.is_some());

    let codec = variant.codec.as_ref().unwrap();
    assert!(codec.codecs.as_deref().unwrap_or("").contains("mp4a.40.2"));
    assert!(
        codec
            .codecs
            .as_deref()
            .unwrap_or("")
            .contains("avc1.64001f")
    );
    // audio_codec might be None even if codecs string contains audio codec
    // Just verify codecs string contains expected values
    let codecs_str = codec.codecs.as_deref().unwrap_or("");
    assert!(codecs_str.contains("mp4a.40.2"));
    assert!(codecs_str.contains("avc1.64001f"));
}

#[test]
fn test_error_types() {
    // Test that HlsError can be converted to string
    let error = HlsError::PlaylistParse("test error".to_string());
    let error_str = error.to_string();
    assert!(error_str.contains("test error"));

    let error = HlsError::VariantNotFound("variant 0".to_string());
    let error_str = error.to_string();
    assert!(error_str.contains("variant 0"));

    let error = HlsError::InvalidUrl("bad url".to_string());
    let error_str = error.to_string();
    assert!(error_str.contains("bad url"));
}

#[test]
fn test_playlist_struct_debug() {
    // Test that playlist structs implement Debug
    let variant = kithara_hls::playlist::VariantStream {
        id: VariantId(0),
        uri: "test.m3u8".to_string(),
        bandwidth: Some(1000000),
        name: Some("Test Variant".to_string()),
        codec: None,
    };

    let debug_output = format!("{:?}", variant);
    assert!(debug_output.contains("VariantStream"));
    assert!(debug_output.contains("test.m3u8"));

    let segment = kithara_hls::playlist::MediaSegment {
        sequence: 0,
        variant_id: VariantId(0),
        uri: "segment.ts".to_string(),
        duration: std::time::Duration::from_secs_f64(4.0),
        key: None,
    };

    let debug_output = format!("{:?}", segment);
    assert!(debug_output.contains("MediaSegment"));
    assert!(debug_output.contains("segment.ts"));
}
