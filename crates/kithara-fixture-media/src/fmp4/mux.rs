use std::num::NonZeroU32;

use kithara_encode::{EncodedAccessUnit, EncodedTrack};
use kithara_signal::rescale_frame_round;
use kithara_stream::AudioCodec;
use num_traits::cast;
use thiserror::Error;

use crate::fmp4::{
    bytes::{Mp4Bytes, full_box, mp4_box, mp4_len},
    codec::CodecDescriptor,
    gapless::GaplessEncoding,
};

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum Fmp4MuxError {
    #[error("codec `{0:?}` does not have an fMP4 descriptor")]
    UnsupportedCodec(AudioCodec),
    #[error("encoded track does not contain access units")]
    EmptyTrack,
    #[error("encoded track is missing required audio metadata")]
    InvalidMediaInfo,
    #[error("fMP4 segment boundary {boundary} exceeds {access_units} access units")]
    InvalidSegmentBoundary {
        boundary: usize,
        access_units: usize,
    },
    #[error("fMP4 segment boundaries must be strictly increasing")]
    UnorderedSegmentBoundaries,
    #[error("fMP4 segment boundaries must name at least one segment")]
    MissingSegmentBoundaries,
    #[error("MP4 timing coordinate exceeds its field range")]
    TimingOverflow,
    #[error(
        "PCM trim {frames} at {sample_rate} Hz is not representable at media timescale {timescale}"
    )]
    UnrepresentableTrim {
        frames: u64,
        sample_rate: u32,
        timescale: u32,
    },
    #[error("gapless trim does not leave a representable presentation")]
    InvalidTrim,
}

/// One track packaged as fMP4: the init segment plus every media segment.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Fmp4Package {
    pub init_segment: Vec<u8>,
    pub media_segments: Vec<Vec<u8>>,
    pub segment_durations_secs: Vec<f64>,
}

impl Fmp4Package {
    /// A package read back from segments that were muxed earlier.
    #[must_use]
    pub fn new(
        init_segment: Vec<u8>,
        media_segments: Vec<Vec<u8>>,
        segment_durations_secs: Vec<f64>,
    ) -> Self {
        Self {
            init_segment,
            media_segments,
            segment_durations_secs,
        }
    }
}

/// The init segment followed by every media segment, which is the shape a
/// decoder reads when it is handed one contiguous fMP4 body. Segment
/// boundaries and durations are dropped: a contiguous body no longer has any.
impl From<Fmp4Package> for Vec<u8> {
    fn from(package: Fmp4Package) -> Self {
        let mut bytes = package.init_segment;
        for segment in package.media_segments {
            bytes.extend_from_slice(&segment);
        }
        bytes
    }
}

/// Packages an encoded track into fMP4 init and media segments.
///
/// # Errors
///
/// Returns an error when the track carries no access units, is missing the
/// audio metadata the boxes need, or uses a codec without an fMP4 descriptor.
pub fn mux_audio_track(
    track: &EncodedTrack,
    gapless_encoding: GaplessEncoding,
) -> Result<Fmp4Package, Fmp4MuxError> {
    mux_audio_track_chunks(
        track,
        gapless_encoding,
        track.access_units.chunks(track.packets_per_segment.max(1)),
    )
}

/// Packages an encoded track at explicit access-unit boundaries.
///
/// Each boundary is the exclusive access-unit index ending one segment. Access
/// units after the last boundary are not packaged, which lets one encode back
/// several fixtures with different durations.
///
/// # Errors
///
/// Returns an error when a boundary is outside the track or the boundaries are
/// not strictly increasing, in addition to [`mux_audio_track`] errors.
pub fn mux_audio_track_at(
    track: &EncodedTrack,
    gapless_encoding: GaplessEncoding,
    boundaries: &[usize],
) -> Result<Fmp4Package, Fmp4MuxError> {
    if track.access_units.is_empty() {
        return Err(Fmp4MuxError::EmptyTrack);
    }
    if boundaries.is_empty() {
        return Err(Fmp4MuxError::MissingSegmentBoundaries);
    }
    let mut previous = 0;
    for &boundary in boundaries {
        if boundary <= previous {
            return Err(Fmp4MuxError::UnorderedSegmentBoundaries);
        }
        if boundary > track.access_units.len() {
            return Err(Fmp4MuxError::InvalidSegmentBoundary {
                boundary,
                access_units: track.access_units.len(),
            });
        }
        previous = boundary;
    }

    let mut start = 0;
    let chunks = boundaries.iter().copied().map(|end| {
        let chunk = &track.access_units[start..end];
        start = end;
        chunk
    });
    mux_audio_track_chunks(track, gapless_encoding, chunks)
}

fn mux_audio_track_chunks<'a>(
    track: &'a EncodedTrack,
    gapless_encoding: GaplessEncoding,
    chunks: impl IntoIterator<Item = &'a [EncodedAccessUnit]>,
) -> Result<Fmp4Package, Fmp4MuxError> {
    if track.access_units.is_empty() {
        return Err(Fmp4MuxError::EmptyTrack);
    }

    let codec = track
        .media_info
        .codec
        .ok_or(Fmp4MuxError::InvalidMediaInfo)?;
    let descriptor =
        CodecDescriptor::for_codec(codec).ok_or(Fmp4MuxError::UnsupportedCodec(codec))?;
    let _ = clock_rates(track)?;
    let _ = track
        .media_info
        .channels
        .filter(|channels| *channels != 0)
        .ok_or(Fmp4MuxError::InvalidMediaInfo)?;

    let chunks = chunks.into_iter().collect::<Vec<_>>();
    let total_duration: u64 =
        chunks
            .iter()
            .flat_map(|chunk| chunk.iter())
            .try_fold(0_u64, |total, unit| {
                total
                    .checked_add(u64::from(unit.duration))
                    .ok_or(Fmp4MuxError::TimingOverflow)
            })?;
    let init_segment = build_init_segment(track, &descriptor, total_duration, gapless_encoding)?;

    let mut media_segments = Vec::new();
    let mut segment_durations_secs = Vec::new();
    let mut decode_time = 0u64;
    let mut sequence_number = 1u32;

    for chunk in chunks {
        let bytes = build_media_segment(chunk, sequence_number, decode_time);
        let duration = chunk
            .iter()
            .map(|sample| u64::from(sample.duration))
            .sum::<u64>();
        media_segments.push(bytes);
        let duration_secs = cast::<u64, f64>(duration).unwrap_or(f64::MAX);
        segment_durations_secs.push(duration_secs / f64::from(track.timescale));
        decode_time = decode_time.saturating_add(duration);
        sequence_number = sequence_number.saturating_add(1);
    }

    Ok(Fmp4Package {
        init_segment,
        media_segments,
        segment_durations_secs,
    })
}

fn build_init_segment(
    track: &EncodedTrack,
    descriptor: &CodecDescriptor,
    total_duration: u64,
    gapless_encoding: GaplessEncoding,
) -> Result<Vec<u8>, Fmp4MuxError> {
    let mut bytes = ftyp_box();
    bytes.extend(moov_box(
        track,
        descriptor,
        total_duration,
        gapless_encoding,
    )?);
    Ok(bytes)
}

fn build_media_segment(
    samples: &[EncodedAccessUnit],
    sequence_number: u32,
    decode_time: u64,
) -> Vec<u8> {
    let moof = moof_box(samples, sequence_number, decode_time, 0);
    let data_offset =
        i32::try_from(moof.len() + 8).expect("`moof` fits the signed 32-bit `trun` data offset");
    let moof = moof_box(samples, sequence_number, decode_time, data_offset);
    let mut bytes = styp_box();
    bytes.extend(moof);
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

fn styp_box() -> Vec<u8> {
    mp4_box(*b"styp", |buf| {
        buf.push_fourcc(*b"iso6");
        buf.push_u32(1);
        buf.push_fourcc(*b"isom");
        buf.push_fourcc(*b"iso6");
        buf.push_fourcc(*b"dash");
    })
}

fn moov_box(
    track: &EncodedTrack,
    descriptor: &CodecDescriptor,
    total_duration: u64,
    gapless_encoding: GaplessEncoding,
) -> Result<Vec<u8>, Fmp4MuxError> {
    let edit = if gapless_encoding.writes_edts() {
        edts_box(track, total_duration)?
    } else {
        None
    };
    let tag = if gapless_encoding.writes_itunsmpb() {
        Some(udta_itunsmpb_box(track, total_duration)?)
    } else {
        None
    };
    Ok(mp4_box(*b"moov", |buf| {
        buf.push_bytes(&mvhd_box(track.timescale, total_duration));
        buf.push_bytes(&trak_box(
            track,
            descriptor,
            total_duration,
            edit.as_deref(),
        ));
        if let Some(tag) = tag {
            buf.push_bytes(&tag);
        }
        buf.push_bytes(&mvex_box());
    }))
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

fn trak_box(
    track: &EncodedTrack,
    descriptor: &CodecDescriptor,
    total_duration: u64,
    edit: Option<&[u8]>,
) -> Vec<u8> {
    mp4_box(*b"trak", |buf| {
        buf.push_bytes(&tkhd_box(total_duration));
        if let Some(edit) = edit {
            buf.push_bytes(edit);
        }
        buf.push_bytes(&mdia_box(track, descriptor, total_duration));
    })
}

fn clock_rates(track: &EncodedTrack) -> Result<(NonZeroU32, NonZeroU32), Fmp4MuxError> {
    let sample_rate = track
        .media_info
        .sample_rate
        .and_then(NonZeroU32::new)
        .ok_or(Fmp4MuxError::InvalidMediaInfo)?;
    let timescale = NonZeroU32::new(track.timescale).ok_or(Fmp4MuxError::InvalidMediaInfo)?;
    Ok((sample_rate, timescale))
}

fn trim_ticks(
    frames: u64,
    sample_rate: NonZeroU32,
    timescale: NonZeroU32,
) -> Result<u64, Fmp4MuxError> {
    let ticks =
        rescale_frame_round(frames, sample_rate, timescale).ok_or(Fmp4MuxError::TimingOverflow)?;
    if rescale_frame_round(ticks, timescale, sample_rate) != Some(frames) {
        return Err(Fmp4MuxError::UnrepresentableTrim {
            frames,
            sample_rate: sample_rate.get(),
            timescale: timescale.get(),
        });
    }
    Ok(ticks)
}

fn edts_box(track: &EncodedTrack, total_duration: u64) -> Result<Option<Vec<u8>>, Fmp4MuxError> {
    let leading = leading_frames(track);
    let trailing = u64::from(track.trailing_delay);
    if leading == 0 && trailing == 0 {
        return Ok(None);
    }
    let (sample_rate, timescale) = clock_rates(track)?;
    let leading = trim_ticks(leading, sample_rate, timescale)?;
    let trailing = trim_ticks(trailing, sample_rate, timescale)?;
    let valid_duration = total_duration
        .checked_sub(leading)
        .and_then(|duration| duration.checked_sub(trailing))
        .ok_or(Fmp4MuxError::InvalidTrim)?;
    if valid_duration == 0 && trailing != 0 {
        return Err(Fmp4MuxError::InvalidTrim);
    }
    let leading = i64::try_from(leading).map_err(|_| Fmp4MuxError::TimingOverflow)?;
    Ok(Some(mp4_box(*b"edts", |buf| {
        buf.push_bytes(&elst_box(valid_duration, leading));
    })))
}

/// Frames before content zero: the requested padding plus the delay the
/// codec's encoder reported.
fn leading_frames(track: &EncodedTrack) -> u64 {
    u64::from(track.encoder_delay) + u64::from(track.codec_delay)
}

fn elst_box(segment_duration: u64, media_time: i64) -> Vec<u8> {
    if let (Ok(duration), Ok(start)) = (u32::try_from(segment_duration), i32::try_from(media_time))
    {
        return full_box(*b"elst", 0, 0, |buf| {
            buf.push_u32(1);
            buf.push_u32(duration);
            buf.push_i32(start);
            buf.push_u16(1);
            buf.push_u16(0);
        });
    }

    full_box(*b"elst", 1, 0, |buf| {
        buf.push_u32(1);
        buf.push_u64(segment_duration);
        buf.push_i64(media_time);
        buf.push_u16(1);
        buf.push_u16(0);
    })
}

/// `moov/udta/meta/ilst/----` carrying the iTunes `iTunSMPB` freeform tag.
///
/// The decoder picks gapless info from this tag when no `elst` is present
/// (see `crates/kithara-decode/src/mp4.rs::parse_itunsmpb`). The iTunes
/// canonical layout for the value is whitespace-separated hex tokens:
/// `<version> <leading> <trailing> <total> ...`. We pad the tail with
/// zero tokens so the string size matches what real-world tooling produces.
fn udta_itunsmpb_box(track: &EncodedTrack, total_duration: u64) -> Result<Vec<u8>, Fmp4MuxError> {
    let leading = leading_frames(track);
    let trailing = u64::from(track.trailing_delay);
    let (sample_rate, timescale) = clock_rates(track)?;
    let total_frames = rescale_frame_round(total_duration, timescale, sample_rate)
        .ok_or(Fmp4MuxError::TimingOverflow)?;
    let total_audible = total_frames
        .checked_sub(leading)
        .and_then(|frames| frames.checked_sub(trailing))
        .ok_or(Fmp4MuxError::InvalidTrim)?;
    let payload = format!(
        " 00000000 {leading:08X} {trailing:08X} {total:016X} \
         00000000 00000000 00000000 00000000 \
         00000000 00000000 00000000 00000000",
        leading = leading,
        trailing = trailing,
        total = total_audible,
    );

    Ok(mp4_box(*b"udta", |udta| {
        udta.push_bytes(&full_box(*b"meta", 0, 0, |meta| {
            meta.push_bytes(&hdlr_meta_box());
            meta.push_bytes(&mp4_box(*b"ilst", |ilst| {
                ilst.push_bytes(&itunsmpb_freeform_box(&payload));
            }));
        }));
    }))
}

fn hdlr_meta_box() -> Vec<u8> {
    full_box(*b"hdlr", 0, 0, |buf| {
        buf.push_u32(0);
        buf.push_fourcc(*b"mdir");
        buf.push_fourcc(*b"appl");
        buf.push_u32(0);
        buf.push_u32(0);
        buf.push_u8(0);
    })
}

fn itunsmpb_freeform_box(payload: &str) -> Vec<u8> {
    mp4_box(*b"----", |buf| {
        buf.push_bytes(&full_box(*b"mean", 0, 0, |mean| {
            mean.push_bytes(b"com.apple.iTunes");
        }));
        buf.push_bytes(&full_box(*b"name", 0, 0, |name| {
            name.push_bytes(b"iTunSMPB");
        }));
        buf.push_bytes(&mp4_box(*b"data", |data| {
            data.push_u32(1);
            data.push_u32(0);
            data.push_bytes(payload.as_bytes());
        }));
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

fn mdia_box(track: &EncodedTrack, descriptor: &CodecDescriptor, total_duration: u64) -> Vec<u8> {
    mp4_box(*b"mdia", |buf| {
        buf.push_bytes(&mdhd_box(track.timescale, total_duration));
        buf.push_bytes(&hdlr_box());
        buf.push_bytes(&minf_box(track, descriptor));
    })
}

fn mdhd_box(timescale: u32, total_duration: u64) -> Vec<u8> {
    let duration = u32::try_from(total_duration).unwrap_or(u32::MAX);
    full_box(*b"mdhd", 0, 0, |buf| {
        buf.push_u32(0);
        buf.push_u32(0);
        buf.push_u32(timescale);
        buf.push_u32(duration);
        buf.push_u16(0x55c4);
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
        buf.push_u32(mp4_len(samples.len()));
        buf.push_i32(data_offset);
        for sample in samples {
            buf.push_u32(sample.duration);
            buf.push_u32(mp4_len(sample.bytes.len()));
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
    use kithara_stream::{ContainerFormat, MediaInfo};
    use kithara_test_utils::kithara;

    use super::*;

    fn test_track() -> EncodedTrack {
        EncodedTrack {
            media_info: MediaInfo::builder()
                .codec(AudioCodec::AacLc)
                .container(ContainerFormat::Fmp4)
                .sample_rate(44_100)
                .channels(2)
                .build(),
            timescale: 44_100,
            bit_rate: 128_000,
            codec_config: Vec::new(),
            packets_per_segment: 2,
            encoder_delay: 0,
            codec_delay: 1024,
            trailing_delay: 0,
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
            media_info: MediaInfo::builder()
                .codec(AudioCodec::Flac)
                .container(ContainerFormat::Fmp4)
                .sample_rate(48_000)
                .channels(2)
                .build(),
            timescale: 48_000,
            bit_rate: 512_000,
            codec_config: vec![
                0x12, 0x00, 0x12, 0x00, 0x00, 0x04, 0x2f, 0x00, 0x09, 0x41, 0x0a, 0xc4, 0x42, 0xf0,
                0x00, 0x00, 0xac, 0x44, 0x09, 0x1a, 0x92, 0x07, 0x6e, 0xc3, 0xbc, 0x84, 0x8e, 0x7f,
                0x60, 0x75, 0x8d, 0x3a, 0x77, 0x61,
            ],
            packets_per_segment: 2,
            encoder_delay: 0,
            codec_delay: 0,
            trailing_delay: 0,
            access_units: vec![
                EncodedAccessUnit {
                    bytes: vec![0xff, 0xf8, 0x69],
                    pts: 0,
                    dts: 0,
                    duration: 4608,
                    is_sync: true,
                },
                EncodedAccessUnit {
                    bytes: vec![0xff, 0xf8, 0x6a],
                    pts: 4608,
                    dts: 4608,
                    duration: 4608,
                    is_sync: true,
                },
            ],
        }
    }

    fn padded_track() -> EncodedTrack {
        let mut track = test_track();
        track.encoder_delay = 2_112;
        track.trailing_delay = 1_920;
        for pts in [4096u64, 5120u64] {
            track.access_units.push(EncodedAccessUnit {
                bytes: vec![0, 0, 0],
                pts,
                dts: pts,
                duration: 1024,
                is_sync: true,
            });
        }
        track
    }

    fn has_marker(init: &[u8], marker: &[u8]) -> bool {
        init.windows(marker.len()).any(|window| window == marker)
    }

    #[kithara::test(native, flash(false))]
    fn init_segment_contains_ftyp_and_moov() {
        let track = test_track();
        let packaged = mux_audio_track(&track, GaplessEncoding::default()).unwrap();
        let init = packaged.init_segment.as_slice();

        assert!(has_marker(init, b"ftyp"));
        assert!(has_marker(init, b"moov"));
        assert!(has_marker(init, b"mp4a"));
    }

    #[kithara::test(native, flash(false))]
    fn media_segments_keep_tfdt_monotonic() {
        let track = test_track();
        let packaged = mux_audio_track(&track, GaplessEncoding::default()).unwrap();
        assert_eq!(packaged.media_segments.len(), 2);
        assert_eq!(&packaged.media_segments[0][4..8], b"styp");
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

    #[kithara::test(native, flash(false))]
    fn explicit_boundaries_package_only_the_named_prefix() {
        let track = test_track();
        let packaged = mux_audio_track_at(&track, GaplessEncoding::None, &[1, 3]).unwrap();

        assert_eq!(packaged.media_segments.len(), 2);
        assert_eq!(
            packaged.segment_durations_secs,
            vec![1024.0 / 44_100.0, 2048.0 / 44_100.0]
        );
    }

    #[kithara::test(native, flash(false))]
    fn init_segment_contains_flac_sample_entry_and_dfla() {
        let track = flac_track();
        let packaged = mux_audio_track(&track, GaplessEncoding::default()).unwrap();
        let init = packaged.init_segment.as_slice();

        assert!(has_marker(init, b"fLaC"));
        assert!(has_marker(init, b"dfLa"));
        assert_eq!(packaged.media_segments.len(), 1);
        assert_eq!(packaged.segment_durations_secs, vec![9216.0 / 48_000.0]);
    }

    #[kithara::test(native, flash(false))]
    fn edts_only_writes_edit_list_and_no_itunsmpb() {
        let packaged = mux_audio_track(&padded_track(), GaplessEncoding::Edts).unwrap();
        let init = packaged.init_segment.as_slice();

        assert!(has_marker(init, b"edts"));
        assert!(has_marker(init, b"elst"));
        assert!(!has_marker(init, b"iTunSMPB"));
        assert!(!has_marker(init, b"udta"));
    }

    #[kithara::test(native, flash(false))]
    fn itunsmpb_only_writes_freeform_tag_and_no_edit_list() {
        let packaged = mux_audio_track(&padded_track(), GaplessEncoding::ItunSmpb).unwrap();
        let init = packaged.init_segment.as_slice();

        assert!(!has_marker(init, b"edts"));
        assert!(!has_marker(init, b"elst"));
        assert!(has_marker(init, b"udta"));
        assert!(has_marker(init, b"meta"));
        assert!(has_marker(init, b"ilst"));
        assert!(has_marker(init, b"----"));
        assert!(has_marker(init, b"mean"));
        assert!(has_marker(init, b"name"));
        assert!(has_marker(init, b"iTunSMPB"));
        assert!(has_marker(init, b"com.apple.iTunes"));
        assert!(has_marker(init, b"00000C40"));
        assert!(has_marker(init, b"00000780"));
    }

    #[kithara::test(native, flash(false))]
    fn both_writes_edit_list_and_itunsmpb() {
        let packaged = mux_audio_track(&padded_track(), GaplessEncoding::Both).unwrap();
        let init = packaged.init_segment.as_slice();

        assert!(has_marker(init, b"edts"));
        assert!(has_marker(init, b"elst"));
        assert!(has_marker(init, b"iTunSMPB"));
    }

    #[kithara::test(native, flash(false))]
    fn none_omits_all_gapless_metadata() {
        let packaged = mux_audio_track(&padded_track(), GaplessEncoding::None).unwrap();
        let init = packaged.init_segment.as_slice();

        assert!(!has_marker(init, b"edts"));
        assert!(!has_marker(init, b"elst"));
        assert!(!has_marker(init, b"iTunSMPB"));
        assert!(!has_marker(init, b"udta"));
    }

    #[kithara::test(native, flash(false))]
    fn unpadded_track_with_edts_encoding_skips_edit_list() {
        let mut track = test_track();
        track.codec_delay = 0;
        let packaged = mux_audio_track(&track, GaplessEncoding::Edts).unwrap();
        let init = packaged.init_segment.as_slice();

        assert!(!has_marker(init, b"edts"));
        assert!(!has_marker(init, b"elst"));
    }

    fn edit_entry(init: &[u8]) -> (u64, i64) {
        let at = init
            .windows(4)
            .position(|bytes| bytes == b"elst")
            .expect("edit list");
        match init[at + 4] {
            0 => (
                u64::from(u32::from_be_bytes(
                    init[at + 12..at + 16].try_into().unwrap(),
                )),
                i64::from(i32::from_be_bytes(
                    init[at + 16..at + 20].try_into().unwrap(),
                )),
            ),
            1 => (
                u64::from_be_bytes(init[at + 12..at + 20].try_into().unwrap()),
                i64::from_be_bytes(init[at + 20..at + 28].try_into().unwrap()),
            ),
            other => panic!("unexpected edit version {other}"),
        }
    }

    #[kithara::test(native)]
    #[case::edts(GaplessEncoding::Edts)]
    #[case::itunsmpb(GaplessEncoding::ItunSmpb)]
    #[case::both(GaplessEncoding::Both)]
    fn padding_fields_use_their_own_clocks(#[case] encoding: GaplessEncoding) {
        let mut track = padded_track();
        track.media_info.sample_rate = Some(48_000);
        track.timescale = 24_000;
        for (index, unit) in track.access_units.iter_mut().enumerate() {
            unit.duration = 512;
            unit.pts = index as u64 * 512;
            unit.dts = unit.pts;
        }
        let package = mux_audio_track(&track, encoding).expect("representable clock domains");
        if encoding.writes_edts() {
            assert_eq!(edit_entry(&package.init_segment), (544, 1568));
        }
        if encoding.writes_itunsmpb() {
            assert!(
                has_marker(
                    &package.init_segment,
                    b" 00000000 00000C40 00000780 0000000000000440 "
                ),
                "SMPB total and padding are PCM counts"
            );
        }
    }

    #[kithara::test(native)]
    fn codec_delay_without_requested_padding_writes_an_edit() {
        let package =
            mux_audio_track(&test_track(), GaplessEncoding::Edts).expect("codec-only delay");
        assert_eq!(edit_entry(&package.init_segment), (3072, 1024));
    }

    #[kithara::test(native)]
    fn a_media_clock_that_cannot_preserve_pcm_trim_is_refused() {
        let mut track = test_track();
        track.media_info.sample_rate = Some(48_000);
        track.timescale = 24_000;
        track.codec_delay = 1025;
        assert!(matches!(
            mux_audio_track(&track, GaplessEncoding::Edts),
            Err(Fmp4MuxError::UnrepresentableTrim {
                frames: 1025,
                sample_rate: 48_000,
                timescale: 24_000
            })
        ));
        let tag =
            mux_audio_track(&track, GaplessEncoding::ItunSmpb).expect("raw PCM-count declaration");
        assert!(has_marker(&tag.init_segment, b"00000401"));
    }

    #[kithara::test(native)]
    fn an_edit_at_the_media_end_keeps_the_open_ended_rule() {
        let mut track = test_track();
        track.access_units.truncate(1);
        let package =
            mux_audio_track(&track, GaplessEncoding::Edts).expect("empty open-ended edit");
        assert_eq!(edit_entry(&package.init_segment), (0, 1024));
        track.trailing_delay = 1;
        assert!(matches!(
            mux_audio_track(&track, GaplessEncoding::Edts),
            Err(Fmp4MuxError::InvalidTrim)
        ));
    }

    #[kithara::test(native)]
    fn wide_edit_fields_keep_their_exact_coordinates() {
        let duration = u64::from(u32::MAX) + 1;
        let media_time = i64::from(i32::MAX) + 1;
        assert_eq!(
            edit_entry(&elst_box(duration, media_time)),
            (duration, media_time)
        );
    }

    #[kithara::test(native)]
    fn untagged_and_equal_clock_bytes_stay_stable() {
        let mut control = Vec::new();
        for track in [test_track(), flac_track()] {
            control.extend(Vec::<u8>::from(
                mux_audio_track(&track, GaplessEncoding::None).expect("untagged control"),
            ));
        }
        for encoding in [
            GaplessEncoding::Edts,
            GaplessEncoding::ItunSmpb,
            GaplessEncoding::Both,
        ] {
            control.extend(Vec::<u8>::from(
                mux_audio_track(&padded_track(), encoding).expect("equal-clock control"),
            ));
        }
        let key = crate::store::formatted_asset_id("mux_controls", "unchanged", &control);
        kithara_test_utils::tracing::warn!(
            key,
            bytes = control.len(),
            "unchanged MP4 wire domains"
        );
        assert_eq!(
            key, "dd9caf25e476d62211f9f042d662e4a5",
            "untagged and equal-clock bytes changed"
        );
    }
}
