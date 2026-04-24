//! Fragmented-MP4 packet reader backed by `symphonia-format-isomp4`.
//!
//! `AudioFile` cannot read packets from an fMP4 stream because the
//! `moov` sample tables are empty — per-fragment descriptors live in
//! `moof/traf/trun`. Rather than hand-rolling a streaming parser, we
//! reuse Symphonia's `IsoMp4Reader` as a pure *container parser*:
//! Symphonia reads boxes lazily from our `MediaSource`, surfaces AAC
//! frames one `Packet` at a time, and we hand those frame bodies to
//! Apple's `AudioConverter` for the actual hardware-accelerated decode.
//!
//! This keeps the streaming contract intact: the reader only consumes
//! bytes as packets are requested, so partially-downloaded HLS sources
//! with LRU eviction and ABR switches work without buffering the whole
//! track up front.

#![allow(unsafe_code)]

use std::{io, time::Duration};

use kithara_bufpool::BytePool;
use symphonia_core::{
    codecs::{
        CodecParameters,
        audio::{
            AudioCodecParameters,
            well_known::{CODEC_ID_AAC, CODEC_ID_FLAC},
        },
    },
    formats::{FormatOptions, FormatReader, SeekMode, SeekTo},
    io::{MediaSource, MediaSourceStream, MediaSourceStreamOptions},
    units::{Time, Timestamp},
};
use symphonia_format_isomp4::IsoMp4Reader;
use tracing::{debug, warn};

use super::{
    consts::{Consts, os_status_to_string},
    ffi::{AudioStreamBasicDescription, AudioStreamPacketDescription},
    reader::{PacketReader, PacketRef},
};
use crate::{
    backend::BoxedSource,
    error::{DecodeError, DecodeResult},
};

/// Adapter turning a `BoxedSource` into a `symphonia_core::MediaSource`.
///
/// The underlying source is a kithara Stream (HLS / File), which is
/// randomly seekable but may block for not-yet-downloaded bytes. We
/// declare it as seekable so Symphonia can random-access moof/mdat
/// atoms, and report the externally-known byte length so the demuxer
/// never has to probe the tail.
struct BoxedMediaSource {
    inner: BoxedSource,
    byte_len: Option<u64>,
}

impl io::Read for BoxedMediaSource {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.inner.read(buf)
    }
}

impl io::Seek for BoxedMediaSource {
    fn seek(&mut self, pos: io::SeekFrom) -> io::Result<u64> {
        self.inner.seek(pos)
    }
}

impl MediaSource for BoxedMediaSource {
    fn is_seekable(&self) -> bool {
        true
    }

    fn byte_len(&self) -> Option<u64> {
        self.byte_len
    }
}

pub(super) struct Fmp4Reader {
    reader: IsoMp4Reader<'static>,
    audio_track_id: u32,
    sample_rate: u32,
    timebase_numer: u32,
    timebase_denom: u32,
    format: AudioStreamBasicDescription,
    magic_cookie: Vec<u8>,
    duration: Option<Duration>,
    /// Reusable scratch for the current packet's bytes so callers get a
    /// `&[u8]` that stays alive until the next `read_next_packet`.
    packet_scratch: Vec<u8>,
    last_packet_desc: AudioStreamPacketDescription,
    eof: bool,
}

impl Fmp4Reader {
    pub(super) fn open(
        source: BoxedSource,
        byte_len: u64,
        _byte_pool: Option<&BytePool>,
    ) -> DecodeResult<Self> {
        let media_source: Box<dyn MediaSource> = Box::new(BoxedMediaSource {
            inner: source,
            byte_len: (byte_len > 0).then_some(byte_len),
        });
        let mss = MediaSourceStream::new(media_source, MediaSourceStreamOptions::default());

        let reader = IsoMp4Reader::try_new(mss, FormatOptions::default())
            .map_err(|e| DecodeError::Backend(Box::new(e)))?;

        let audio = reader
            .tracks()
            .iter()
            .find(|t| {
                t.codec_params
                    .as_ref()
                    .is_some_and(|p| matches!(p, CodecParameters::Audio(_)))
            })
            .ok_or_else(|| DecodeError::InvalidData("fmp4: no audio track".into()))?;

        let audio_track_id = audio.id;
        let Some(CodecParameters::Audio(audio_params)) = audio.codec_params.as_ref() else {
            return Err(DecodeError::InvalidData(
                "fmp4: audio track missing codec params".into(),
            ));
        };

        let sample_rate = audio_params.sample_rate.ok_or_else(|| {
            DecodeError::InvalidData("fmp4: audio track missing sample rate".into())
        })?;
        let channel_count = audio_params.channels.as_ref().map_or(2u16, |ch| {
            #[expect(
                clippy::cast_possible_truncation,
                reason = "channel count fits in u16 for valid audio"
            )]
            let c = ch.count().max(1) as u16;
            c
        });

        let (format, magic_cookie) = build_input_format(audio_params, sample_rate, channel_count)?;

        let (timebase_numer, timebase_denom) = audio.time_base.map_or_else(
            || (1u32, sample_rate.max(1)),
            |tb| (tb.numer.get(), tb.denom.get()),
        );

        // `num_frames` is in the track's own time base (mdhd timescale), not
        // audio sample units, so dividing by `sample_rate` would overestimate
        // the duration whenever the container picked a timescale different
        // from the audio sample rate (e.g. mp4a tracks with timescale=90000).
        // Use the time-base API for the correct conversion.
        let duration = audio.num_frames.and_then(|n| {
            #[expect(
                clippy::cast_possible_wrap,
                reason = "num_frames stays well under i64::MAX for realistic durations"
            )]
            let ts = Timestamp::new(n as i64);
            audio.time_base.and_then(|tb| tb.calc_time(ts)).map(|t| {
                #[expect(
                    clippy::cast_precision_loss,
                    reason = "nanos values below 2^53 are exactly representable"
                )]
                let secs = (t.as_nanos() as f64) / 1e9;
                Duration::from_secs_f64(secs.max(0.0))
            })
        });

        debug!(
            audio_track_id,
            sample_rate,
            channel_count,
            format_id = %os_status_to_string(format.mFormatID.cast_signed()),
            frames_per_packet = format.mFramesPerPacket,
            cookie_len = magic_cookie.len(),
            ?duration,
            "Fmp4Reader opened"
        );

        Ok(Self {
            reader,
            audio_track_id,
            sample_rate,
            timebase_numer,
            timebase_denom,
            format,
            magic_cookie,
            duration,
            packet_scratch: Vec::new(),
            last_packet_desc: AudioStreamPacketDescription::default(),
            eof: false,
        })
    }
}

/// Build the Apple `AudioStreamBasicDescription` and magic cookie for the
/// codec discovered inside the fMP4 sample entry.
///
/// fMP4 can carry multiple codecs; Apple's `AudioConverter` only decodes
/// correctly when the ASBD advertises the real format and the cookie matches
/// what that codec's decoder expects. Dispatching off
/// `AudioCodecParameters::codec` (the Symphonia-parsed sample-entry fourcc)
/// is the authoritative source — more reliable than the upstream
/// `MediaInfo` hint which can be stale or absent.
fn build_input_format(
    audio_params: &AudioCodecParameters,
    sample_rate: u32,
    channels: u16,
) -> DecodeResult<(AudioStreamBasicDescription, Vec<u8>)> {
    let extra: &[u8] = audio_params.extra_data.as_deref().unwrap_or_default();

    match audio_params.codec {
        CODEC_ID_AAC => {
            if extra.is_empty() {
                warn!("fmp4 aac: extra_data missing (no AudioSpecificConfig cookie)");
            }
            let asbd = AudioStreamBasicDescription {
                mSampleRate: f64::from(sample_rate),
                mFormatID: Consts::kAudioFormatMPEG4AAC,
                mFormatFlags: 0,
                mBytesPerPacket: 0,
                mFramesPerPacket: Consts::AAC_FRAMES_PER_PACKET,
                mBytesPerFrame: 0,
                mChannelsPerFrame: u32::from(channels),
                mBitsPerChannel: 0,
                mReserved: 0,
            };
            Ok((asbd, extra.to_vec()))
        }
        CODEC_ID_FLAC => {
            if extra.len() < Consts::FLAC_STREAMINFO_LEN {
                return Err(DecodeError::InvalidData(format!(
                    "flac fmp4: STREAMINFO too short ({} bytes, need {})",
                    extra.len(),
                    Consts::FLAC_STREAMINFO_LEN,
                )));
            }
            let streaminfo = &extra[..Consts::FLAC_STREAMINFO_LEN];
            // max_block_size is u16 BE at bytes [2..4] of STREAMINFO.
            let max_block = u32::from(u16::from_be_bytes([streaminfo[2], streaminfo[3]]))
                .max(Consts::AAC_FRAMES_PER_PACKET);

            // Apple's FLAC decoder expects the magic cookie in native-FLAC
            // header form:
            //   "fLaC" + METADATA_BLOCK_HEADER(last=1, type=STREAMINFO, len=34)
            //         + STREAMINFO body.
            // Symphonia surfaces only the STREAMINFO body, so we rebuild the
            // prefix here.
            let mut cookie =
                Vec::with_capacity(Consts::FLAC_COOKIE_PREFIX_LEN + Consts::FLAC_STREAMINFO_LEN);
            cookie.extend_from_slice(b"fLaC");
            cookie.push(0x80); // last=1, type=0 (STREAMINFO)
            cookie.push(0x00);
            cookie.push(0x00);
            #[expect(
                clippy::cast_possible_truncation,
                reason = "Consts::FLAC_STREAMINFO_LEN is 34, fits in u8"
            )]
            {
                cookie.push(Consts::FLAC_STREAMINFO_LEN as u8);
            }
            cookie.extend_from_slice(streaminfo);

            let asbd = AudioStreamBasicDescription {
                mSampleRate: f64::from(sample_rate),
                mFormatID: Consts::kAudioFormatFLAC,
                mFormatFlags: 0,
                mBytesPerPacket: 0,
                mFramesPerPacket: max_block,
                mBytesPerFrame: 0,
                mChannelsPerFrame: u32::from(channels),
                mBitsPerChannel: 0,
                mReserved: 0,
            };
            Ok((asbd, cookie))
        }
        other => Err(DecodeError::InvalidData(format!(
            "fmp4: unsupported codec id {other} in Apple fmp4 reader"
        ))),
    }
}

impl PacketReader for Fmp4Reader {
    fn format(&self) -> AudioStreamBasicDescription {
        self.format
    }

    fn magic_cookie(&self) -> Option<&[u8]> {
        if self.magic_cookie.is_empty() {
            None
        } else {
            Some(&self.magic_cookie)
        }
    }

    fn duration(&self) -> Option<Duration> {
        self.duration
    }

    fn read_next_packet(&mut self) -> DecodeResult<Option<PacketRef<'_>>> {
        if self.eof {
            return Ok(None);
        }
        loop {
            let packet = match self.reader.next_packet() {
                Ok(Some(p)) => p,
                Ok(None) => {
                    self.eof = true;
                    return Ok(None);
                }
                Err(symphonia_core::errors::Error::IoError(e))
                    if e.kind() == io::ErrorKind::UnexpectedEof =>
                {
                    self.eof = true;
                    return Ok(None);
                }
                Err(e) => {
                    return Err(DecodeError::Backend(Box::new(e)));
                }
            };
            if packet.track_id() != self.audio_track_id {
                continue;
            }
            let data = packet.buf();
            self.packet_scratch.clear();
            self.packet_scratch.extend_from_slice(data);
            #[expect(
                clippy::cast_possible_truncation,
                reason = "AAC packet body fits in u32"
            )]
            let size = self.packet_scratch.len() as u32;
            self.last_packet_desc = AudioStreamPacketDescription {
                mStartOffset: 0,
                mVariableFramesInPacket: 0,
                mDataByteSize: size,
            };
            return Ok(Some(PacketRef {
                data: &self.packet_scratch,
                description: self.last_packet_desc,
            }));
        }
    }

    fn seek_to_frame(&mut self, target_frame: u64) -> DecodeResult<u64> {
        #[expect(
            clippy::cast_precision_loss,
            reason = "frame counts below 2^53 are exactly representable"
        )]
        let target_secs = target_frame as f64 / f64::from(self.sample_rate.max(1));
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "seconds fit in i64 for realistic durations"
        )]
        let time = Time::try_new(target_secs as i64, (target_secs.fract() * 1e9) as u32)
            .unwrap_or(Time::ZERO);
        let result = self
            .reader
            .seek(
                SeekMode::Accurate,
                SeekTo::Time {
                    time,
                    track_id: Some(self.audio_track_id),
                },
            )
            .map_err(|e| DecodeError::SeekFailed(e.to_string()))?;
        self.eof = false;

        // Translate the seeked-to timestamp back to a frame count in
        // the audio's output sample rate via the track's timebase.
        #[expect(
            clippy::cast_sign_loss,
            reason = "actual_ts is non-negative after a successful seek"
        )]
        let aligned_ts = result.actual_ts.get().max(0) as u64;
        let aligned_frame = if self.timebase_denom > 0 {
            #[expect(
                clippy::cast_possible_truncation,
                reason = "frame counts stay well under u64::MAX"
            )]
            {
                (u128::from(aligned_ts)
                    * u128::from(self.sample_rate)
                    * u128::from(self.timebase_numer)
                    / u128::from(self.timebase_denom.max(1))) as u64
            }
        } else {
            aligned_ts
        };
        Ok(aligned_frame)
    }
}
