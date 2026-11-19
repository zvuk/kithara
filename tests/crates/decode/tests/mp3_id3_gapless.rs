use std::io::Cursor;

use kithara::{
    decode::{DecoderBackend, DecoderChunkOutcome, DecoderConfig, DecoderFactory, GaplessTrimmer},
    platform::time::Duration,
    resampler::NoResamplerBackend,
    stream::id3v2_tag_len,
};
use kithara_test_utils::bufpool::{TestPools, pools};

fn id3(version: u8, footer: bool, payload: &[u8]) -> Vec<u8> {
    let size = u32::try_from(payload.len()).expect("test tag fits u32");
    let mut header = [
        b'I',
        b'D',
        b'3',
        version,
        0,
        u8::from(footer) * 0x10,
        0,
        0,
        0,
        0,
    ];
    for (byte, shift) in header[6..].iter_mut().zip([21, 14, 7, 0]) {
        *byte = ((size >> shift) & 0x7f) as u8;
    }
    let mut bytes = header.to_vec();
    bytes.extend_from_slice(payload);
    if footer {
        bytes.extend_from_slice(b"3DI");
        bytes.extend_from_slice(&header[3..]);
    }
    bytes
}

fn sync_payload(length: usize) -> Vec<u8> {
    let mut payload = vec![0; length];
    payload[..4].copy_from_slice(&[0xff, 0xfb, 0x90, 0x64]);
    payload
}

fn decode_visible(bytes: Vec<u8>) -> Vec<f32> {
    #[cfg(not(target_os = "android"))]
    let backend = DecoderBackend::Symphonia;
    #[cfg(target_os = "android")]
    let backend = DecoderBackend::default();
    let config = DecoderConfig::<NoResamplerBackend, TestPools>::builder()
        .pools(pools())
        .backend(backend)
        .build();
    let mut decoder = DecoderFactory::create_with_probe(Cursor::new(bytes), Some("mp3"), config)
        .expect("create MP3 decoder");
    let mut trimmer = decoder
        .track_info()
        .gapless
        .map_or_else(GaplessTrimmer::disabled, GaplessTrimmer::from);
    let mut samples = Vec::new();
    loop {
        match decoder.next_chunk().expect("decode MP3 chunk") {
            DecoderChunkOutcome::Chunk(chunk) => {
                for chunk in trimmer.push(*chunk).expect("trim MP3 chunk") {
                    samples.extend_from_slice(chunk.as_ref());
                }
            }
            DecoderChunkOutcome::Pending(_) => panic!("complete MP3 input must not be pending"),
            DecoderChunkOutcome::Eof => break,
        }
    }
    for chunk in trimmer.flush().expect("flush MP3 trim") {
        samples.extend_from_slice(chunk.as_ref());
    }
    samples
}

#[kithara::test(native, timeout(Duration::from_secs(2)), hang_timeout_secs(1))]
#[case::one_small_v23("one_small_v23", id3(3, false, &sync_payload(64)))]
#[case::oversized_v23("oversized_v23", id3(3, false, &sync_payload(32 * 1024)))]
#[case::footer_then_oversized("footer_then_oversized", [
    id3(4, true, &[0; 64]),
    id3(3, false, &sync_payload(32 * 1024)),
].concat())]
#[case::three_small_false_sync("three_small_false_sync", [
    id3(3, false, &[0; 64]),
    id3(4, false, &[0; 64]),
    id3(3, false, &sync_payload(64)),
].concat())]
#[case::two_oversized_tags("two_oversized_tags", [
    id3(3, false, &[0; 32 * 1024]),
    id3(3, false, &[0; 32 * 1024]),
].concat())]
#[case::chain_ends_near_16k("chain_ends_near_16k", [
    id3(3, false, &[0; 64]),
    id3(3, false, &[0; 16_260]),
].concat())]
#[case::header_across_16k("header_across_16k", [
    id3(3, false, &[0; 16_369]),
    id3(3, false, &sync_payload(64)),
].concat())]
fn mp3_visible_pcm_ignores_leading_id3_tags(#[case] label: &str, #[case] mut tagged: Vec<u8>) {
    let asset = kithara_test_fixtures::assets::signal_mp3_sine1k_48k_1s();
    let mut body = asset.bytes();
    while let Some(length) = id3v2_tag_len(body) {
        body = &body[usize::try_from(length).expect("test tag fits usize")..];
    }
    let reference = decode_visible(body.to_vec());
    let frames = reference.len() / 2;
    assert!(
        frames.abs_diff(48_000) <= 2,
        "expected 48000 frames +/-2, got {frames}"
    );
    tagged.extend_from_slice(body);
    let actual = decode_visible(tagged);
    assert!(
        actual == reference,
        "{label}: visible PCM differs ({} vs {} samples)",
        actual.len(),
        reference.len()
    );
}
