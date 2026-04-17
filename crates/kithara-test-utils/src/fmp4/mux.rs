use std::{borrow::Cow, sync::Arc};

use kithara_encode::{EncodedAccessUnit, EncodedTrack};
use kithara_stream::AudioCodec;
use thiserror::Error;

use crate::fmp4::{
    bytes::{Mp4Bytes, full_box, mp4_box},
    codec::CodecDescriptor,
};

#[derive(Debug, Error)]
pub(crate) enum PackagedMuxError {
    #[error("codec `{0:?}` does not have an fMP4 descriptor")]
    UnsupportedCodec(AudioCodec),
    #[error("encoded track does not contain access units")]
    EmptyTrack,
    #[error("encoded track is missing required audio metadata")]
    InvalidMediaInfo,
}

#[derive(Debug, Clone)]
pub(crate) struct PackagedVariantData {
    pub(crate) rfc6381_codec: Cow<'static, str>,
    pub(crate) init_segment: Arc<Vec<u8>>,
    pub(crate) media_segments: Vec<Arc<Vec<u8>>>,
    pub(crate) segment_durations_secs: Vec<f64>,
}

pub(crate) fn mux_audio_track(
    track: &EncodedTrack,
) -> Result<PackagedVariantData, PackagedMuxError> {
    if track.access_units.is_empty() {
        return Err(PackagedMuxError::EmptyTrack);
    }

    let codec = track
        .media_info
        .codec
        .ok_or(PackagedMuxError::InvalidMediaInfo)?;
    let descriptor =
        CodecDescriptor::for_codec(codec).ok_or(PackagedMuxError::UnsupportedCodec(codec))?;
    let rfc6381_codec = track
        .media_info
        .rfc6381_codec()
        .ok_or(PackagedMuxError::UnsupportedCodec(codec))?;
    let _ = track
        .media_info
        .sample_rate
        .ok_or(PackagedMuxError::InvalidMediaInfo)?;
    let _ = track
        .media_info
        .channels
        .ok_or(PackagedMuxError::InvalidMediaInfo)?;

    let total_duration: u64 = track
        .access_units
        .iter()
        .map(|au| u64::from(au.duration))
        .sum();
    let init_segment = Arc::new(build_init_segment(track, &descriptor, total_duration));

    let mut media_segments = Vec::new();
    let mut segment_durations_secs = Vec::new();
    let mut decode_time = 0u64;
    let mut sequence_number = 1u32;

    for chunk in track.access_units.chunks(track.packets_per_segment.max(1)) {
        let bytes = build_media_segment(chunk, sequence_number, decode_time);
        let duration = chunk
            .iter()
            .map(|sample| u64::from(sample.duration))
            .sum::<u64>();
        media_segments.push(Arc::new(bytes));
        segment_durations_secs.push(duration as f64 / f64::from(track.timescale));
        decode_time = decode_time.saturating_add(duration);
        sequence_number = sequence_number.saturating_add(1);
    }

    Ok(PackagedVariantData {
        rfc6381_codec,
        init_segment,
        media_segments,
        segment_durations_secs,
    })
}

fn build_init_segment(
    track: &EncodedTrack,
    descriptor: &CodecDescriptor,
    total_duration: u64,
) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend(ftyp_box());
    bytes.extend(moov_box(track, descriptor, total_duration));
    bytes
}

fn build_media_segment(
    samples: &[EncodedAccessUnit],
    sequence_number: u32,
    decode_time: u64,
) -> Vec<u8> {
    let moof = moof_box(samples, sequence_number, decode_time, 0);
    let data_offset = (moof.len() + 8) as i32;
    let moof = moof_box(samples, sequence_number, decode_time, data_offset);
    let mut bytes = moof;
    bytes.extend(mdat_box(samples));
    bytes
}

fn ftyp_box() -> Vec<u8> {
    mp4_box(*b"ftyp", |buf| {
        buf.push_fourcc(*b"isom");
        buf.push_u32(0x0000_0200);
        buf.push_fourcc(*b"isom");
        buf.push_fourcc(*b"iso6");
        buf.push_fourcc(*b"mp41");
    })
}

fn moov_box(track: &EncodedTrack, descriptor: &CodecDescriptor, total_duration: u64) -> Vec<u8> {
    mp4_box(*b"moov", |buf| {
        buf.push_bytes(&mvhd_box(track.timescale, total_duration));
        buf.push_bytes(&trak_box(track, descriptor, total_duration));
        buf.push_bytes(&mvex_box());
    })
}

fn mvhd_box(timescale: u32, total_duration: u64) -> Vec<u8> {
    let duration = u32::try_from(total_duration).unwrap_or(u32::MAX);
    full_box(*b"mvhd", 0, 0, |buf| {
        buf.push_u32(0);
        buf.push_u32(0);
        buf.push_u32(timescale);
        buf.push_u32(duration);
        buf.push_u32(0x0001_0000);
        buf.push_u16(0x0100);
        buf.push_zeroes(10);
        push_identity_matrix(buf);
        buf.push_zeroes(24);
        buf.push_u32(2);
    })
}

fn trak_box(track: &EncodedTrack, descriptor: &CodecDescriptor, total_duration: u64) -> Vec<u8> {
    mp4_box(*b"trak", |buf| {
        buf.push_bytes(&tkhd_box(total_duration));
        buf.push_bytes(&mdia_box(track, descriptor));
    })
}

fn tkhd_box(total_duration: u64) -> Vec<u8> {
    let duration = u32::try_from(total_duration).unwrap_or(u32::MAX);
    full_box(*b"tkhd", 0, 0x000007, |buf| {
        buf.push_u32(0);
        buf.push_u32(0);
        buf.push_u32(1);
        buf.push_u32(0);
        buf.push_u32(duration);
        buf.push_zeroes(8);
        buf.push_u16(0);
        buf.push_u16(0);
        buf.push_u16(0x0100);
        buf.push_u16(0);
        push_identity_matrix(buf);
        buf.push_u32(0);
        buf.push_u32(0);
    })
}

fn mdia_box(track: &EncodedTrack, descriptor: &CodecDescriptor) -> Vec<u8> {
    mp4_box(*b"mdia", |buf| {
        buf.push_bytes(&mdhd_box(track.timescale));
        buf.push_bytes(&hdlr_box());
        buf.push_bytes(&minf_box(track, descriptor));
    })
}

fn mdhd_box(timescale: u32) -> Vec<u8> {
    full_box(*b"mdhd", 0, 0, |buf| {
        buf.push_u32(0);
        buf.push_u32(0);
        buf.push_u32(timescale);
        buf.push_u32(0);
        buf.push_u16(0x55C4);
        buf.push_u16(0);
    })
}

fn hdlr_box() -> Vec<u8> {
    full_box(*b"hdlr", 0, 0, |buf| {
        buf.push_u32(0);
        buf.push_fourcc(*b"soun");
        buf.push_zeroes(12);
        buf.push_bytes(b"SoundHandler\0");
    })
}

fn minf_box(track: &EncodedTrack, descriptor: &CodecDescriptor) -> Vec<u8> {
    mp4_box(*b"minf", |buf| {
        buf.push_bytes(&smhd_box());
        buf.push_bytes(&dinf_box());
        buf.push_bytes(&stbl_box(track, descriptor));
    })
}

fn smhd_box() -> Vec<u8> {
    full_box(*b"smhd", 0, 0, |buf| {
        buf.push_u16(0);
        buf.push_u16(0);
    })
}

fn dinf_box() -> Vec<u8> {
    mp4_box(*b"dinf", |buf| {
        buf.push_bytes(&dref_box());
    })
}

fn dref_box() -> Vec<u8> {
    full_box(*b"dref", 0, 0, |buf| {
        buf.push_u32(1);
        buf.push_bytes(&full_box(*b"url ", 0, 0x000001, |_| {}));
    })
}

fn stbl_box(track: &EncodedTrack, descriptor: &CodecDescriptor) -> Vec<u8> {
    mp4_box(*b"stbl", |buf| {
        buf.push_bytes(&stsd_box(track, descriptor));
        buf.push_bytes(&full_box(*b"stts", 0, 0, |stts| stts.push_u32(0)));
        buf.push_bytes(&full_box(*b"stsc", 0, 0, |stsc| stsc.push_u32(0)));
        buf.push_bytes(&full_box(*b"stsz", 0, 0, |stsz| {
            stsz.push_u32(0);
            stsz.push_u32(0);
        }));
        buf.push_bytes(&full_box(*b"stco", 0, 0, |stco| stco.push_u32(0)));
    })
}

fn stsd_box(track: &EncodedTrack, descriptor: &CodecDescriptor) -> Vec<u8> {
    let sample_rate = track
        .media_info
        .sample_rate
        .expect("validated in mux_audio_track");
    let channels = track
        .media_info
        .channels
        .expect("validated in mux_audio_track");
    let sample_entry =
        descriptor.sample_entry(sample_rate, channels, track.bit_rate, &track.codec_config);
    full_box(*b"stsd", 0, 0, |buf| {
        buf.push_u32(1);
        buf.push_bytes(&sample_entry);
    })
}

fn mvex_box() -> Vec<u8> {
    mp4_box(*b"mvex", |buf| {
        buf.push_bytes(&trex_box());
    })
}

fn trex_box() -> Vec<u8> {
    full_box(*b"trex", 0, 0, |buf| {
        buf.push_u32(1);
        buf.push_u32(1);
        buf.push_u32(0);
        buf.push_u32(0);
        buf.push_u32(0);
    })
}

fn moof_box(
    samples: &[EncodedAccessUnit],
    sequence_number: u32,
    decode_time: u64,
    data_offset: i32,
) -> Vec<u8> {
    mp4_box(*b"moof", |buf| {
        buf.push_bytes(&mfhd_box(sequence_number));
        buf.push_bytes(&traf_box(samples, decode_time, data_offset));
    })
}

fn mfhd_box(sequence_number: u32) -> Vec<u8> {
    full_box(*b"mfhd", 0, 0, |buf| {
        buf.push_u32(sequence_number);
    })
}

fn traf_box(samples: &[EncodedAccessUnit], decode_time: u64, data_offset: i32) -> Vec<u8> {
    mp4_box(*b"traf", |buf| {
        buf.push_bytes(&tfhd_box());
        buf.push_bytes(&tfdt_box(decode_time));
        buf.push_bytes(&trun_box(samples, data_offset));
    })
}

fn tfhd_box() -> Vec<u8> {
    full_box(*b"tfhd", 0, 0x020000, |buf| {
        buf.push_u32(1);
    })
}

fn tfdt_box(decode_time: u64) -> Vec<u8> {
    full_box(*b"tfdt", 1, 0, |buf| {
        buf.push_u64(decode_time);
    })
}

fn trun_box(samples: &[EncodedAccessUnit], data_offset: i32) -> Vec<u8> {
    full_box(*b"trun", 0, 0x000301, |buf| {
        buf.push_u32(samples.len() as u32);
        buf.push_i32(data_offset);
        for sample in samples {
            buf.push_u32(sample.duration);
            buf.push_u32(sample.bytes.len() as u32);
        }
    })
}

fn mdat_box(samples: &[EncodedAccessUnit]) -> Vec<u8> {
    mp4_box(*b"mdat", |buf| {
        for sample in samples {
            buf.push_bytes(&sample.bytes);
        }
    })
}

fn push_identity_matrix(buf: &mut Mp4Bytes) {
    for value in [0x0001_0000, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x4000_0000] {
        buf.push_u32(value);
    }
}

#[cfg(test)]
mod tests {
    use kithara_encode::{EncodedAccessUnit, EncodedTrack};
    use kithara_stream::{ContainerFormat, MediaInfo};

    use super::*;
    use crate::kithara;

    fn test_track() -> EncodedTrack {
        EncodedTrack {
            media_info: MediaInfo::default()
                .with_codec(AudioCodec::AacLc)
                .with_container(ContainerFormat::Fmp4)
                .with_sample_rate(44_100)
                .with_channels(2),
            timescale: 44_100,
            bit_rate: 128_000,
            codec_config: Vec::new(),
            packets_per_segment: 2,
            access_units: vec![
                EncodedAccessUnit {
                    bytes: vec![1, 2, 3],
                    pts: 0,
                    dts: 0,
                    duration: 1024,
                    is_sync: true,
                },
                EncodedAccessUnit {
                    bytes: vec![4, 5, 6],
                    pts: 1024,
                    dts: 1024,
                    duration: 1024,
                    is_sync: true,
                },
                EncodedAccessUnit {
                    bytes: vec![7, 8, 9],
                    pts: 2048,
                    dts: 2048,
                    duration: 1024,
                    is_sync: true,
                },
                EncodedAccessUnit {
                    bytes: vec![10, 11, 12],
                    pts: 3072,
                    dts: 3072,
                    duration: 1024,
                    is_sync: true,
                },
            ],
        }
    }

    fn flac_track() -> EncodedTrack {
        EncodedTrack {
            media_info: MediaInfo::default()
                .with_codec(AudioCodec::Flac)
                .with_container(ContainerFormat::Fmp4)
                .with_sample_rate(48_000)
                .with_channels(2),
            timescale: 48_000,
            bit_rate: 512_000,
            codec_config: vec![
                0x12, 0x00, 0x12, 0x00, 0x00, 0x04, 0x2F, 0x00, 0x09, 0x41, 0x0A, 0xC4, 0x42, 0xF0,
                0x00, 0x00, 0xAC, 0x44, 0x09, 0x1A, 0x92, 0x07, 0x6E, 0xC3, 0xBC, 0x84, 0x8E, 0x7F,
                0x60, 0x75, 0x8D, 0x3A, 0x77, 0x61,
            ],
            packets_per_segment: 2,
            access_units: vec![
                EncodedAccessUnit {
                    bytes: vec![0xFF, 0xF8, 0x69],
                    pts: 0,
                    dts: 0,
                    duration: 4608,
                    is_sync: true,
                },
                EncodedAccessUnit {
                    bytes: vec![0xFF, 0xF8, 0x6A],
                    pts: 4608,
                    dts: 4608,
                    duration: 4608,
                    is_sync: true,
                },
            ],
        }
    }

    #[kithara::test]
    fn init_segment_contains_ftyp_and_moov() {
        let track = test_track();
        let packaged = mux_audio_track(&track).unwrap();
        let init = packaged.init_segment.as_slice();

        assert!(init.windows(4).any(|window| window == b"ftyp"));
        assert!(init.windows(4).any(|window| window == b"moov"));
        assert!(init.windows(4).any(|window| window == b"mp4a"));
        assert_eq!(packaged.rfc6381_codec.as_ref(), "mp4a.40.2");
    }

    #[kithara::test]
    fn media_segments_keep_tfdt_monotonic() {
        let track = test_track();
        let packaged = mux_audio_track(&track).unwrap();
        assert_eq!(packaged.media_segments.len(), 2);
        assert!(
            packaged.media_segments[0]
                .windows(4)
                .any(|window| window == b"moof")
        );
        assert!(
            packaged.media_segments[0]
                .windows(4)
                .any(|window| window == b"mdat")
        );
        assert!(
            packaged.segment_durations_secs[1] >= packaged.segment_durations_secs[0] - f64::EPSILON
        );
    }

    #[kithara::test]
    fn init_segment_contains_flac_sample_entry_and_dfla() {
        let track = flac_track();
        let packaged = mux_audio_track(&track).unwrap();
        let init = packaged.init_segment.as_slice();

        assert!(init.windows(4).any(|window| window == b"fLaC"));
        assert!(init.windows(4).any(|window| window == b"dfLa"));
        assert_eq!(packaged.rfc6381_codec.as_ref(), "flac");
        assert_eq!(packaged.media_segments.len(), 1);
        assert_eq!(packaged.segment_durations_secs, vec![9216.0 / 48_000.0]);
    }
}
