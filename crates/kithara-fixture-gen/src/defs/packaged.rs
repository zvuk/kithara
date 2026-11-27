#![cfg(feature = "packaged")]

use std::{num::NonZeroU32, sync::OnceLock};

use kithara_bufpool::{OverallBudget, Pool, PoolConfig, PoolRegion, PoolSlot};
use kithara_encode::{EncodedTrack, EncoderFactory, PackagedEncodeRequest};
use kithara_signal::rescale_frame_round;
use kithara_stream::{AudioCodec, ContainerFormat, MediaInfo};
use kithara_test_macros as kithara;

use crate::{
    defs::wav::{RhythmControl, rhythm_pcm},
    fmp4::{Fmp4Package, GaplessEncoding, mux_audio_track},
    signal::{Pcm, Wave},
};

#[derive(Pool)]
pub(super) struct FixturePools {
    bytes: PoolSlot<u8>,
    samples: PoolSlot<f32>,
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
    let prepared = super::rhythm::source_pcm(&[&inputs.concat()], "mp4");
    super::rhythm::analysed(&[&prepared])
}

fn aac_track(codec: AudioCodec, bit_rate: u64) -> EncodedTrack {
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

    EncoderFactory::encode_packaged(
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
    .unwrap_or_else(|error| panic!("kithara-test-fixtures: {codec:?} encode failed: {error}"))
}

fn aac_bytes(codec: AudioCodec, bit_rate: u64) -> Vec<u8> {
    let track = aac_track(codec, bit_rate);
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

#[kithara::asset(ext = "mp4", content_type = "audio/mp4", embed)]
#[case::declared(true)]
#[case::undeclared(false)]
fn he_aac_implicit(declared: bool) -> Vec<u8> {
    static TRACK: OnceLock<EncodedTrack> = OnceLock::new();
    let mut track = TRACK
        .get_or_init(|| aac_track(AudioCodec::AacHe, consts::AAC_HE_BIT_RATE))
        .clone();
    let output_rate = NonZeroU32::new(consts::SAMPLE_RATE).expect("fixture output rate");
    let core_rate = NonZeroU32::new(consts::SAMPLE_RATE / 2).expect("fixture core rate");
    for count in [
        &mut track.encoder_delay,
        &mut track.codec_delay,
        &mut track.trailing_delay,
    ] {
        *count = u32::try_from(
            rescale_frame_round(u64::from(*count), output_rate, core_rate)
                .expect("representable core-frame count"),
        )
        .expect("fixture trim fits u32");
    }
    track.media_info.sample_rate = Some(core_rate.get());
    track.codec_config = vec![0x13, 0x90];
    let encoding = if declared {
        GaplessEncoding::ItunSmpb
    } else {
        GaplessEncoding::None
    };
    Vec::from(mux_audio_track(&track, encoding).expect("implicit-core HE fixture"))
}

#[cfg(all(test, feature = "rhythm"))]
mod tests {
    use kithara_test_utils::kithara;

    #[kithara::test]
    fn packaged_analysis_keeps_both_halves_and_the_native_decoder_profile() {
        for deck in ["a", "b"] {
            let name = format!("analysis_rhythm_fmp4_deck_{deck}_120bpm_48k");
            let definition = inventory::iter::<crate::registry::AssetDef>
                .into_iter()
                .find(|definition| definition.accessor_name() == name)
                .expect("packaged whole-track analysis");
            assert_eq!(
                definition.dependencies,
                [
                    format!("rhythm_fmp4_init_deck_{deck}_120bpm_48k"),
                    format!("rhythm_fmp4_media_deck_{deck}_120bpm_48k"),
                ]
            );
            let format = definition.format.expect("profile-keyed packaged analysis")();
            assert!(format.ends_with(&super::super::rhythm::source_format()));
        }
    }
}
