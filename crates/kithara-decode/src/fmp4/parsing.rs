use std::io::{self, Cursor, Error};

use kithara_bufpool::{HasPool, PoolRegion};
use kithara_platform::time::Duration;
use kithara_stream::AudioCodec;
use re_mp4::{Mp4, StsdBoxContent};

use super::sample::{extract_aac_asc_raw, parse_flac_sample_entry};
use crate::{
    consts,
    error::{DecodeError, DecodeResult},
};

/// Codec-specific decoder config bytes carried in the init segment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CodecConfig {
    /// AAC `AudioSpecificConfig` bytes (`ESDS` `DecoderSpecificInfo` body).
    Aac(Vec<u8>),
    /// FLAC `STREAMINFO` block payload (34 bytes, no metadata header).
    Flac([u8; consts::FLAC_STREAMINFO_BYTES]),
}

impl AsRef<[u8]> for CodecConfig {
    fn as_ref(&self) -> &[u8] {
        match self {
            Self::Aac(bytes) => bytes,
            Self::Flac(bytes) => bytes,
        }
    }
}

/// Parsed init segment. Holds everything a segment-level codec needs
/// to decode subsequent media segments.
#[derive(Debug, Clone)]
pub(crate) struct Fmp4InitInfo {
    pub(crate) codec: AudioCodec,
    pub(crate) config: CodecConfig,
    /// Container-level gapless info derived from the init segment
    /// (`elst` edit-list trim or `udta` `iTunSMPB`). `None` when the
    /// init blob carries neither — codec-side capture (Apple `PrimeInfo`
    /// refresh) supplements this when the codec exposes priming.
    pub(crate) gapless: Option<crate::GaplessInfo>,
    pub(crate) channels: u16,
    pub(crate) sample_rate: u32,
    pub(crate) timescale: u32,
    pub(crate) track_id: u32,
}

/// Per-frame view into a single media segment's buffer.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Fmp4Frame {
    /// Frame duration in ticks.
    pub(crate) duration: u32,
    /// Absolute decode time in `init.timescale` ticks.
    pub(crate) decode_time: u64,
    /// Offset of frame bytes inside the segment buffer.
    pub(crate) offset: usize,
    /// Frame byte size.
    pub(crate) size: usize,
}

/// Parse an `EXT-X-MAP` init segment.
pub(crate) fn parse_init<S>(bytes: &[u8], pools: &PoolRegion<S>) -> DecodeResult<Fmp4InitInfo>
where
    S: HasPool<u8>,
{
    let mp4 = Mp4::read_bytes(bytes).map_err(|e| DecodeError::parse("re_mp4", e))?;

    let track_box = mp4
        .moov
        .traks
        .iter()
        .find(|trak| {
            matches!(trak.mdia.minf.stbl.stsd.contents, StsdBoxContent::Mp4a(_))
                || matches!(
                    trak.mdia.minf.stbl.stsd.contents,
                    StsdBoxContent::Unknown(_)
                )
        })
        .ok_or_else(|| DecodeError::InvalidData {
            detail: "no audio trak in init segment",
        })?;

    let timescale = track_box.mdia.mdhd.timescale;
    let track_id = track_box.tkhd.track_id;

    let (codec, sample_rate, channels, config) = match &track_box.mdia.minf.stbl.stsd.contents {
        StsdBoxContent::Mp4a(mp4a) => {
            let sample_rate = u32::from(mp4a.samplerate.value());
            let channels = mp4a.channelcount;
            let asc = extract_aac_asc_raw(bytes)?;
            (
                AudioCodec::AacLc,
                sample_rate,
                channels,
                CodecConfig::Aac(asc),
            )
        }
        StsdBoxContent::Unknown(fourcc) if u32::from(*fourcc) == consts::FOURCC_FLAC => {
            let (sample_rate, channels, streaminfo) = parse_flac_sample_entry(bytes)?;
            (
                AudioCodec::Flac,
                sample_rate,
                channels,
                CodecConfig::Flac(streaminfo),
            )
        }
        _ => {
            return Err(DecodeError::InvalidData {
                detail: "unsupported audio sample entry",
            });
        }
    };

    let gapless = {
        let mut cursor = Cursor::new(bytes);
        crate::gapless::probe_mp4_gapless(&mut cursor, pools).unwrap_or(None)
    };

    Ok(Fmp4InitInfo {
        codec,
        config,
        gapless,
        channels,
        sample_rate,
        timescale,
        track_id,
    })
}

impl Fmp4InitInfo {
    /// Time of the native sample a `timescale` tick count stands for.
    ///
    /// A timescale that does not count native samples rounds each sample
    /// time to its nearest tick, so a tick count stands for the sample
    /// nearest to it. Every packet then starts on one sample lattice, and
    /// every variant of a source places a sample at the same time. A track
    /// without a sample rate keeps the tick time.
    pub(crate) fn time_at(&self, ticks: u64) -> Duration {
        let timescale = u128::from(self.timescale);
        if timescale == 0 {
            return Duration::ZERO;
        }
        let ticks = u128::from(ticks);
        let (units, per_second) = match self.sample_rate {
            0 => (ticks, self.timescale),
            rate => (
                (ticks * u128::from(rate) * 2 + timescale) / (timescale * 2),
                rate,
            ),
        };
        let whole = u64::try_from(units / u128::from(per_second)).unwrap_or(u64::MAX);
        let part = u64::try_from(units % u128::from(per_second)).unwrap_or(u64::MAX);
        Duration::from_secs(whole).saturating_add(Duration::from_secs(part) / per_second)
    }
}

/// Walk a media segment's `(moof, mdat)` pairs and emit per-frame
/// descriptors. The returned offsets are relative to `segment_bytes`.
///
/// The box walk itself belongs to `kithara-mp4`; what stays here is the
/// projection of its samples onto the buffer-relative view the demuxer
/// slices frames out of.
///
/// The frame vector is presized and filled by hand, since collecting into a `Result<Vec<_>>` would
/// lose the exact capacity, and a segment must cost exactly one allocation.
pub(crate) fn parse_segment_frames(
    init: &Fmp4InitInfo,
    segment_bytes: &[u8],
) -> DecodeResult<Vec<Fmp4Frame>> {
    let total = u64::try_from(segment_bytes.len()).map_err(|_| DecodeError::InvalidData {
        detail: "segment length overflows u64",
    })?;
    let samples = kithara_mp4::read_samples(&SegmentBytes(segment_bytes), total, init.track_id)
        .map_err(|error| DecodeError::InvalidData {
            detail: error.detail(),
        })?;
    let mut frames: Vec<Fmp4Frame> = Vec::with_capacity(samples.len());
    for sample in &samples {
        frames.push(frame_from_sample(sample)?);
    }
    Ok(frames)
}

/// Random-access view of one media segment already held in memory. The walk
/// takes a [`kithara_mp4::ReadAt`] because it is written for sources it must
/// not pull whole; a segment buffer simply answers from the slice it is.
struct SegmentBytes<'a>(&'a [u8]);

impl kithara_mp4::ReadAt for SegmentBytes<'_> {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        let start = usize::try_from(offset).map_err(Error::other)?;
        let Some(tail) = self.0.get(start..) else {
            return Ok(0);
        };
        let n = tail.len().min(buf.len());
        buf[..n].copy_from_slice(&tail[..n]);
        Ok(n)
    }
}

/// Project one walked sample onto the segment buffer the demuxer slices.
fn frame_from_sample(sample: &kithara_mp4::Sample) -> DecodeResult<Fmp4Frame> {
    let offset =
        usize::try_from(sample.byte_range.start).map_err(|_| DecodeError::InvalidData {
            detail: "frame offset overflows usize",
        })?;
    let size = usize::try_from(sample.byte_range.end - sample.byte_range.start).map_err(|_| {
        DecodeError::InvalidData {
            detail: "frame size overflows usize",
        }
    })?;
    Ok(Fmp4Frame {
        offset,
        size,
        decode_time: sample.decode_ticks,
        duration: sample.duration_ticks,
    })
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use kithara_platform::time::Duration;
    use kithara_test_fixtures::unit_fixtures::{aac_init, aac_segment, flac_init};
    use kithara_test_utils::kithara;

    use super::*;
    use crate::test_pools::pools;

    #[kithara::test]
    fn parse_init_aac_extracts_codec_and_asc(aac_init: Vec<u8>) {
        let bytes = aac_init;
        let init = parse_init(&bytes, &pools()).expect("BUG: parse init");
        assert_eq!(init.codec, AudioCodec::AacLc);
        assert!(init.timescale > 0, "timescale={}", init.timescale);
        assert!(init.sample_rate >= 8_000 && init.sample_rate <= 96_000);
        assert!(init.channels >= 1 && init.channels <= 8);
        let asc = init.config.as_ref();
        assert!(
            asc.len() == 2 || asc.len() == 5,
            "ASC length unexpected: {} bytes",
            asc.len()
        );
        let aot = asc[0] >> 3;
        assert_eq!(aot, 2, "expected AAC-LC AOT=2, got {aot}");
    }

    #[kithara::test]
    fn parse_init_flac_extracts_streaminfo(flac_init: Vec<u8>) {
        let bytes = flac_init;
        let init = parse_init(&bytes, &pools()).expect("BUG: parse FLAC init");
        assert_eq!(init.codec, AudioCodec::Flac);
        assert!(matches!(init.config, CodecConfig::Flac(_)));
        let len = init.config.as_ref().len();
        assert_eq!(len, 34, "STREAMINFO body must be 34 bytes");
    }

    #[kithara::test]
    fn parse_segment_frames_aac_yields_monotonic_frames(aac_init: Vec<u8>, aac_segment: Vec<u8>) {
        let init_bytes = aac_init;
        let init = parse_init(&init_bytes, &pools()).expect("BUG: parse init");
        let seg_bytes = aac_segment;
        let frames = parse_segment_frames(&init, &seg_bytes).expect("BUG: parse seg");
        assert!(
            frames.len() > 40,
            "expected ≥40 frames, got {}",
            frames.len()
        );

        for pair in frames.windows(2) {
            let (a, b) = (&pair[0], &pair[1]);
            assert!(
                b.decode_time > a.decode_time,
                "non-monotonic decode_time: {} -> {}",
                a.decode_time,
                b.decode_time
            );
        }
        for f in &frames {
            assert!(
                f.offset + f.size <= seg_bytes.len(),
                "frame {}+{} > seg {}",
                f.offset,
                f.size,
                seg_bytes.len()
            );
            assert!(f.size > 0);
        }
    }

    /// R-remp4: the per-frame `Vec<Fmp4Frame>` must be presized from the
    /// `trun` sample count, so a single-moof segment is built with exactly
    /// one allocation — capacity equals the frame count, no realloc churn.
    #[kithara::test]
    fn parse_segment_frames_presizes_vec_to_sample_count(aac_init: Vec<u8>, aac_segment: Vec<u8>) {
        let init_bytes = aac_init;
        let init = parse_init(&init_bytes, &pools()).expect("BUG: parse init");
        let seg_bytes = aac_segment;
        let frames = parse_segment_frames(&init, &seg_bytes).expect("BUG: parse seg");
        assert!(!frames.is_empty(), "segment must yield frames");
        assert_eq!(
            frames.capacity(),
            frames.len(),
            "Vec<Fmp4Frame> must be presized to the trun sample count \
             (exact capacity, single allocation)",
        );
    }

    #[kithara::test]
    fn parse_segment_frames_total_duration_matches_extinf(aac_init: Vec<u8>, aac_segment: Vec<u8>) {
        let init_bytes = aac_init;
        let init = parse_init(&init_bytes, &pools()).expect("BUG: parse init");
        let seg_bytes = aac_segment;
        let frames = parse_segment_frames(&init, &seg_bytes).expect("BUG: parse seg");
        let total_ticks: u64 = frames.iter().map(|f| u64::from(f.duration)).sum();
        let total_seconds =
            Duration::from_nanos(total_ticks * 1_000_000_000 / u64::from(init.timescale))
                .as_secs_f64();
        assert!(
            total_seconds > 5.0 && total_seconds < 7.0,
            "segment duration off: {total_seconds}s (timescale={})",
            init.timescale
        );
    }
}
