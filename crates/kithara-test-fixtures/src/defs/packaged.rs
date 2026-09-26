#![cfg(feature = "packaged")]

use std::sync::OnceLock;

use kithara_bufpool::{OverallBudget, PoolConfig, PoolRegion, pool_schema};
use kithara_encode::{EncoderFactory, PackagedEncodeRequest};
use kithara_stream::{AudioCodec, ContainerFormat, MediaInfo};
use kithara_test_macros as kithara;

use crate::{
    defs::wav::{RhythmControl, rhythm_pcm},
    fmp4::{Fmp4Package, GaplessEncoding, mux_audio_track},
    signal::{Pcm, Wave},
};

pool_schema! {
    pub(super) FixturePools {
        bytes: u8,
        samples: f32,
    }
}

pub(super) fn pools() -> PoolRegion<FixturePools> {
    FixturePools::builder(OverallBudget(64 * 1024 * 1024))
        .bytes(PoolConfig::builder().max_buffers(32).build())
        .samples(PoolConfig::builder().max_buffers(8).build())
        .build()
        .unwrap_or_else(|error| panic!("kithara-test-fixtures: pool region failed: {error}"))
}

mod consts {
    pub(super) const AAC_HE_BIT_RATE: u64 = 64_000;
    pub(super) const AAC_HE_V2_BIT_RATE: u64 = 32_000;
    pub(super) const CHANNELS: u16 = 2;
    pub(super) const ONE_SECOND_FRAMES: usize = 44_100;
    pub(super) const RHYTHM_BIT_RATE: u64 = 512_000;
    pub(super) const RHYTHM_FRAMES: usize = 576_000;
    pub(super) const RHYTHM_SAMPLE_RATE: u32 = 48_000;
    pub(super) const SAMPLE_RATE: u32 = 44_100;
}

static RHYTHM_FMP4: [OnceLock<Fmp4Package>; 2] = [OnceLock::new(), OnceLock::new()];

fn rhythm_fmp4(index: usize, carrier_hz: f64) -> &'static Fmp4Package {
    RHYTHM_FMP4[index].get_or_init(|| {
        let codec = AudioCodec::Flac;
        let frame_samples = EncoderFactory::frame_samples(codec).unwrap_or_else(|error| {
            panic!("kithara-test-fixtures: FLAC frame size failed: {error}")
        });
        let packets = consts::RHYTHM_FRAMES.div_ceil(frame_samples);
        let pcm = rhythm_pcm(
            consts::RHYTHM_SAMPLE_RATE,
            consts::CHANNELS,
            packets * frame_samples,
            120.0,
            carrier_hz,
            0,
            RhythmControl::Aligned,
        );
        let media_info = MediaInfo::builder()
            .codec(codec)
            .container(ContainerFormat::Fmp4)
            .sample_rate(consts::RHYTHM_SAMPLE_RATE)
            .channels(consts::CHANNELS)
            .build();
        let pools = pools();
        let track = EncoderFactory::encode_packaged(
            &pools,
            &PackagedEncodeRequest::builder()
                .pcm(&pcm)
                .media_info(media_info)
                .timescale(consts::RHYTHM_SAMPLE_RATE)
                .bit_rate(consts::RHYTHM_BIT_RATE)
                .packets_per_segment(packets)
                .encoder_delay(0)
                .trailing_delay(0)
                .build(),
        )
        .unwrap_or_else(|error| {
            panic!("kithara-test-fixtures: rhythmic FLAC encode failed: {error}")
        });
        mux_audio_track(&track, GaplessEncoding::None).unwrap_or_else(|error| {
            panic!("kithara-test-fixtures: rhythmic fMP4 mux failed: {error}")
        })
    })
}

#[kithara::asset(ext = "mp4", content_type = "audio/mp4", fragment)]
#[case::deck_a_120bpm_48k(0, 220.0)]
#[case::deck_b_120bpm_48k(1, 880.0)]
fn rhythm_fmp4_init(index: usize, carrier_hz: f64) -> Vec<u8> {
    rhythm_fmp4(index, carrier_hz).init_segment.clone()
}

#[kithara::asset(ext = "m4s", content_type = "audio/mp4", fragment)]
#[case::deck_a_120bpm_48k(0, 220.0)]
#[case::deck_b_120bpm_48k(1, 880.0)]
fn rhythm_fmp4_media(index: usize, carrier_hz: f64) -> Vec<u8> {
    rhythm_fmp4(index, carrier_hz)
        .media_segments
        .first()
        .cloned()
        .expect("rhythmic fMP4 has one media segment")
}

/// The deck the init and media halves play as, analysed as one track.
#[cfg(feature = "rhythm")]
#[kithara::asset(
    ext = "analysis",
    content_type = "application/x-kithara-analysis",
    format = super::rhythm::analysed_format,
    depends_on = ["rhythm_fmp4_init_{case}", "rhythm_fmp4_media_{case}"]
)]
#[case::deck_a_120bpm_48k()]
#[case::deck_b_120bpm_48k()]
fn analysis_rhythm_fmp4(inputs: &[&[u8]]) -> Vec<u8> {
    super::rhythm::analysed(&[&inputs.concat()], "mp4")
}

fn aac_bytes(codec: AudioCodec, bit_rate: u64) -> Vec<u8> {
    let frame_samples = EncoderFactory::frame_samples(codec).unwrap_or_else(|error| {
        panic!("kithara-test-fixtures: {codec:?} has no packaged frame size: {error}")
    });
    let packets = consts::ONE_SECOND_FRAMES.div_ceil(frame_samples);
    let pcm = Pcm::new(
        consts::SAMPLE_RATE,
        consts::CHANNELS,
        packets * frame_samples,
        Wave::Sawtooth,
    );
    let media_info = MediaInfo::builder()
        .codec(codec)
        .container(ContainerFormat::Fmp4)
        .sample_rate(consts::SAMPLE_RATE)
        .channels(consts::CHANNELS)
        .build();

    let track = EncoderFactory::encode_packaged(
        &pools(),
        &PackagedEncodeRequest::builder()
            .pcm(&pcm)
            .media_info(media_info)
            .timescale(consts::SAMPLE_RATE)
            .bit_rate(bit_rate)
            .packets_per_segment(packets)
            .encoder_delay(0)
            .trailing_delay(0)
            .build(),
    )
    .unwrap_or_else(|error| panic!("kithara-test-fixtures: {codec:?} encode failed: {error}"));

    Vec::from(
        mux_audio_track(&track, GaplessEncoding::None).unwrap_or_else(|error| {
            panic!("kithara-test-fixtures: {codec:?} fMP4 mux failed: {error}")
        }),
    )
}

/// An AAC-LC saw packaged as one complete fMP4 body for browser tests.
///
/// Embedded because wasm has no fixture store.
#[kithara::asset(ext = "mp4", content_type = "audio/mp4", embed)]
#[case::lc()]
fn aac() -> Vec<u8> {
    aac_bytes(AudioCodec::AacLc, consts::AAC_HE_BIT_RATE)
}

/// An HE-AAC saw packaged as one complete fMP4 body for browser tests.
///
/// Embedded because wasm has no fixture store.
#[kithara::asset(ext = "mp4", content_type = "audio/mp4", embed)]
#[case::v1(AudioCodec::AacHe, consts::AAC_HE_BIT_RATE)]
#[case::v2(AudioCodec::AacHeV2, consts::AAC_HE_V2_BIT_RATE)]
fn he_aac(codec: AudioCodec, bit_rate: u64) -> Vec<u8> {
    aac_bytes(codec, bit_rate)
}
