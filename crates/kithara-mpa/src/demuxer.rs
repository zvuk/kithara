// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::io::{ErrorKind, Seek, SeekFrom};

use num_traits::ToPrimitive;
use symphonia_core::{
    codecs::{CodecParameters, audio::AudioCodecParameters},
    errors::{Error, Result, SeekErrorKind, decode_error, seek_error},
    formats::{
        prelude::*,
        probe::{ProbeFormatData, ProbeableFormat, Score, Scoreable},
        well_known::{FORMAT_ID_MP1, FORMAT_ID_MP2, FORMAT_ID_MP3},
    },
    io::*,
    meta::{Metadata, MetadataLog},
    packet::PacketRef,
    support_format,
};
use tracing::{debug, info, warn};

use crate::{
    common::{FrameHeader, MpegLayer},
    header::{self, MAX_MPEG_FRAME_SIZE, MPEG_HEADER_LEN},
    tags::{is_maybe_info_tag, is_maybe_vbri_tag, try_read_info_tag, try_read_vbri_tag},
};

struct FormatInfos;

impl FormatInfos {
    const MP1: FormatInfo = FormatInfo {
        format: FORMAT_ID_MP1,
        short_name: "mp1",
        long_name: "MPEG Audio Layer 1 Native",
    };
    const MP2: FormatInfo = FormatInfo {
        format: FORMAT_ID_MP2,
        short_name: "mp2",
        long_name: "MPEG Audio Layer 2 Native",
    };
    const MP3: FormatInfo = FormatInfo {
        format: FORMAT_ID_MP3,
        short_name: "mp3",
        long_name: "MPEG Audio Layer 3 Native",
    };
}

/// MPEG1 and MPEG2 audio elementary stream reader.
///
/// `MpaReader` implements a demuxer for the MPEG1 and MPEG2 audio elementary stream.
pub struct MpaReader<'s> {
    format_info: &'static FormatInfo,
    media_info: MediaInfo,
    reader: MediaSourceStream<'s>,
    metadata: MetadataLog,
    chapters: Option<ChapterGroup>,
    next_packet_ts: Timestamp,
    tracks: Vec<Track>,
    first_packet_pos: u64,
}

impl Scoreable for MpaReader<'_> {
    fn score(mut src: ScopedStream<&mut MediaSourceStream<'_>>) -> Result<Score> {
        const FULL_CONFIDENCE: u8 = 255;
        const PARTIAL_CONFIDENCE: u8 = 127;

        let sync1 = header::read_frame_header_word_no_sync(&mut src)?;
        let hdr1 = header::parse_frame_header(sync1)?;

        if src.bytes_available() < (hdr1.frame_size + MPEG_HEADER_LEN) as u64 {
            return Ok(Score::Supported(PARTIAL_CONFIDENCE));
        }

        src.ignore_bytes(hdr1.frame_size as u64)?;

        let sync2 = header::read_frame_header_word_no_sync(&mut src)?;

        if !header::is_frame_header_word_synced(sync2) {
            return Ok(Score::Unsupported);
        }

        let _ = header::parse_frame_header(sync2)?;

        Ok(Score::Supported(FULL_CONFIDENCE))
    }
}

impl ProbeableFormat<'_> for MpaReader<'_> {
    fn probe_data() -> &'static [ProbeFormatData] {
        &[
            support_format!(
                FormatInfos::MP1,
                &["mp1"],
                &["audio/mpeg", "audio/mp1"],
                &[
                    &[0xff, 0xfe],
                    &[0xff, 0xff],
                    &[0xff, 0xf6],
                    &[0xff, 0xf7],
                    &[0xff, 0xe6],
                    &[0xff, 0xe7],
                ]
            ),
            support_format!(
                FormatInfos::MP2,
                &["mp2"],
                &["audio/mpeg", "audio/mp2"],
                &[
                    &[0xff, 0xfc],
                    &[0xff, 0xfd],
                    &[0xff, 0xf4],
                    &[0xff, 0xf5],
                    &[0xff, 0xe4],
                    &[0xff, 0xe5],
                ]
            ),
            support_format!(
                FormatInfos::MP3,
                &["mp3"],
                &["audio/mpeg", "audio/mp3"],
                &[
                    &[0xff, 0xfa],
                    &[0xff, 0xfb],
                    &[0xff, 0xf2],
                    &[0xff, 0xf3],
                    &[0xff, 0xe2],
                    &[0xff, 0xe3],
                ]
            ),
        ]
    }

    fn try_probe_new(
        mss: MediaSourceStream<'_>,
        opts: FormatOptions,
    ) -> Result<Box<dyn FormatReader + '_>> {
        Ok(Box::new(MpaReader::try_new(mss, opts)?))
    }
}

impl FormatReader for MpaReader<'_> {
    fn chapters(&self) -> Option<&ChapterGroup> {
        self.chapters.as_ref()
    }

    fn format_info(&self) -> &FormatInfo {
        self.format_info
    }

    fn into_inner<'s>(self: Box<Self>) -> MediaSourceStream<'s>
    where
        Self: 's,
    {
        self.reader
    }

    fn media_info(&self) -> &MediaInfo {
        &self.media_info
    }

    fn metadata(&mut self) -> Metadata<'_> {
        self.metadata.metadata()
    }

    fn next_packet(&mut self) -> Result<Option<Packet>> {
        let mut scratch = [0; MAX_MPEG_FRAME_SIZE];
        Ok(self.next_packet_ref(&mut scratch)?.map(|packet| {
            let mut owned = Packet::new(packet.track_id, packet.pts, packet.dur, packet.data);
            owned.dts = packet.dts;
            owned.trim_start = packet.trim_start;
            owned.trim_end = packet.trim_end;
            owned
        }))
    }

    fn seek(&mut self, mode: SeekMode, to: SeekTo) -> Result<SeekedTo> {
        let mut packet_buf = [0; MAX_MPEG_FRAME_SIZE];
        self.seek_with_buffer(mode, &to, &mut packet_buf)
    }

    fn tracks(&self) -> &[Track] {
        &self.tracks
    }
}

impl<'s> MpaReader<'s> {
    /// Reads a packet into caller-provided bounded storage.
    ///
    /// # Errors
    /// Returns the source or MPEG framing error without advancing a transient read.
    fn next_packet_ref<'a>(&mut self, packet_buf: &'a mut [u8]) -> Result<Option<PacketRef<'a>>> {
        let header = loop {
            let header = match read_mpeg_frame_into(&mut self.reader, packet_buf) {
                Ok(header) => header,
                Err(Error::IoError(err)) if err.kind() == ErrorKind::UnexpectedEof => {
                    return Ok(None);
                }
                Err(err) => return Err(err),
            };

            let data = &packet_buf[..MPEG_HEADER_LEN + header.frame_size];
            if is_maybe_info_tag(data, &header) {
                if try_read_info_tag(data, &header).is_some() {
                    warn!("found an unexpected xing tag, discarding");
                    continue;
                }
            } else if is_maybe_vbri_tag(data, &header) && try_read_vbri_tag(data, &header).is_some()
            {
                warn!("found an unexpected vbri tag, discarding");
                continue;
            }

            break header;
        };

        let pts = self.next_packet_ts;
        let dur = header.duration();

        self.next_packet_ts = match self.next_packet_ts.checked_add(dur) {
            Some(ts) => ts,
            None => return Ok(None),
        };

        let packet = PacketBuilder::new()
            .track_id(0)
            .pts(pts)
            .trimmed_dur(
                dur,
                self.tracks[0]
                    .num_frames
                    .map(Duration::from)
                    .and_then(|dur| dur.timestamp_from(Timestamp::ZERO)),
            )
            .data_by_ref(&packet_buf[..MPEG_HEADER_LEN + header.frame_size])
            .build_packet_ref();

        Ok(Some(packet))
    }

    /// Seeks the media source stream back to the start of the first packet if the required
    /// timestamp is in the past.
    fn preseek_accurate(&mut self, required_ts: Timestamp, min_ts: Timestamp) -> Result<()> {
        if required_ts < self.next_packet_ts {
            let seeked_pos = self.reader.seek(SeekFrom::Start(self.first_packet_pos))?;

            if seeked_pos != self.first_packet_pos {
                return seek_error(SeekErrorKind::Unseekable);
            }

            self.next_packet_ts = min_ts;
        }

        Ok(())
    }

    fn preseek_coarse(
        &mut self,
        required_ts: Timestamp,
        min_ts: Timestamp,
        max_ts: Option<Timestamp>,
        packet_buf: &mut [u8],
    ) -> Result<()> {
        let audio_byte_len = match self.reader.byte_len() {
            Some(byte_len) => u128::from(byte_len - self.first_packet_pos),
            None => return seek_error(SeekErrorKind::Unseekable),
        };

        let max_ts = match max_ts {
            Some(max_ts) if max_ts >= min_ts => max_ts,
            _ => return seek_error(SeekErrorKind::Unseekable),
        };

        debug_assert!(min_ts <= required_ts);
        debug_assert!(required_ts <= max_ts);

        let total_dur = u128::from(max_ts.abs_delta(min_ts).get());

        let seek_pos_rel =
            (u128::from(required_ts.abs_delta(min_ts).get()) * audio_byte_len) / total_dur;

        let seek_pos = (seek_pos_rel + u128::from(self.first_packet_pos))
            .saturating_sub(MAX_MPEG_FRAME_SIZE as u128);

        self.reader.seek(SeekFrom::Start(
            seek_pos
                .try_into()
                .map_err(|_| Error::SeekError(SeekErrorKind::OutOfRange))?,
        ))?;

        let header = read_mpeg_frame_strict_into(&mut self.reader, packet_buf)?;

        let audio_byte_pos = u128::from(self.reader.pos() - self.first_packet_pos);

        let dur_to_pkt = Duration::from(
            u64::try_from((audio_byte_pos * total_dur) / audio_byte_len)
                .map_err(|_| Error::SeekError(SeekErrorKind::OutOfRange))?,
        );

        let pkt_dur = header.duration();

        let aligned = dur_to_pkt
            .align_down(pkt_dur)
            .ok_or(Error::SeekError(SeekErrorKind::OutOfRange))?;
        self.next_packet_ts = min_ts.checked_add(aligned).unwrap_or(max_ts);

        Ok(())
    }

    /// Seeks the media source stream to a byte position roughly where the packet with the required
    /// timestamp should be located.
    /// Parses frames forward from the current position until the frame holding
    /// `required_ts` is reached, leaving the reader on the reference frame a
    /// decoder needs to resume from.
    fn scan_to(&mut self, required_ts: Timestamp) -> Result<()> {
        const MAX_REF_FRAMES: usize = 4;
        const REF_FRAMES_MASK: usize = MAX_REF_FRAMES - 1;

        let mut frames: [FramePos; MAX_REF_FRAMES] = Default::default();
        let mut n_parsed = 0;

        loop {
            self.reader.ensure_seekback_buffer(MAX_MPEG_FRAME_SIZE);
            let checkpoint = self.reader.pos();

            let synced = header::sync_frame(&mut self.reader);
            let sync = match roll_back_transient(&mut self.reader, checkpoint, synced) {
                Ok(sync) => sync,
                Err(Error::IoError(err)) if err.kind() == ErrorKind::UnexpectedEof => {
                    return seek_error(SeekErrorKind::OutOfRange);
                }
                Err(err) => return Err(err),
            };

            let header = header::parse_frame_header(sync)?;

            let pos = self.reader.pos() - MPEG_HEADER_LEN as u64;

            let frame_dur = header.duration();

            frames[n_parsed & REF_FRAMES_MASK] = FramePos {
                pos,
                ts: self.next_packet_ts,
            };
            n_parsed += 1;

            let next_packet_ts = match self.next_packet_ts.checked_add(frame_dur) {
                Some(ts) if ts <= required_ts => ts,
                _ => {
                    let read = read_main_data_begin(&mut self.reader, &header);
                    let main_data_begin =
                        u64::from(roll_back_transient(&mut self.reader, checkpoint, read)?);

                    debug!(
                        "found frame with ts={} @ pos={} with main_data_begin={}",
                        self.next_packet_ts, pos, main_data_begin
                    );

                    let mut n_ref_frames = 0;
                    let mut ref_frame = &frames[(n_parsed - 1) & REF_FRAMES_MASK];

                    if main_data_begin > 0 {
                        let max_ref_frames = std::cmp::min(n_parsed, frames.len());

                        while n_ref_frames < max_ref_frames {
                            ref_frame = &frames[(n_parsed - n_ref_frames - 1) & REF_FRAMES_MASK];

                            if pos - ref_frame.pos >= main_data_begin {
                                break;
                            }

                            n_ref_frames += 1;
                        }

                        debug!(
                            "will seek -{} frame(s) to ts={} @ pos={} (-{} bytes)",
                            n_ref_frames,
                            ref_frame.ts,
                            ref_frame.pos,
                            pos - ref_frame.pos
                        );
                    }

                    self.reader.seek_buffered(ref_frame.pos);

                    self.next_packet_ts = ref_frame.ts;
                    break;
                }
            };

            let ignored = self
                .reader
                .ignore_bytes(header.frame_size as u64)
                .map_err(Error::from);
            roll_back_transient(&mut self.reader, checkpoint, ignored)?;

            self.next_packet_ts = next_packet_ts;
        }

        Ok(())
    }

    /// Seeks using caller-provided packet scratch storage.
    ///
    /// # Errors
    /// Returns source, framing, or seek errors.
    fn seek_with_buffer(
        &mut self,
        mode: SeekMode,
        to: &SeekTo,
        packet_buf: &mut [u8],
    ) -> Result<SeekedTo> {
        let required_ts = match *to {
            SeekTo::Timestamp { ts, .. } => ts,
            SeekTo::Time { time, .. } => {
                let tb = self.tracks[0]
                    .time_base
                    .ok_or(Error::SeekError(SeekErrorKind::Unseekable))?;

                tb.calc_timestamp(time)
                    .ok_or(Error::SeekError(SeekErrorKind::OutOfRange))?
            }
        };

        let dur_ts = self.tracks[0].num_frames.map(Duration::from);

        let delay = self.tracks[0].delay.unwrap_or(0);
        let padding = self.tracks[0].padding.unwrap_or(0);

        let min_ts = Timestamp::from(-i64::from(delay));
        let max_ts = dur_ts
            .and_then(|dur| min_ts.checked_add(dur))
            .and_then(|dur| dur.checked_add(Duration::from(delay + padding)));

        if required_ts < min_ts {
            return seek_error(SeekErrorKind::OutOfRange);
        } else if let Some(max_ts) = max_ts
            && required_ts > max_ts
        {
            return seek_error(SeekErrorKind::OutOfRange);
        }

        let is_seekable = self.reader.is_seekable();

        if !is_seekable && required_ts < self.next_packet_ts {
            return seek_error(SeekErrorKind::ForwardOnly);
        }

        debug!("seeking to ts={required_ts}");

        match mode {
            SeekMode::Coarse if is_seekable => {
                self.preseek_coarse(required_ts, min_ts, max_ts, packet_buf)?;
            }
            SeekMode::Accurate => self.preseek_accurate(required_ts, min_ts)?,
            SeekMode::Coarse => (),
        }

        self.scan_to(required_ts)?;

        debug!(
            "seeked to ts={} (delta={})",
            self.next_packet_ts,
            self.next_packet_ts.saturating_delta(required_ts),
        );

        Ok(SeekedTo {
            required_ts,
            track_id: 0,
            actual_ts: self.next_packet_ts,
        })
    }

    /// Reads the first MPEG frame to identify the layer and build the track.
    ///
    /// # Errors
    ///
    /// Returns a decode error when no MPEG frame can be synchronised, or the
    /// underlying I/O error when the source cannot be read.
    pub fn try_new(mut mss: MediaSourceStream<'s>, opts: FormatOptions) -> Result<Self> {
        let mut packet = [0; MAX_MPEG_FRAME_SIZE];
        let header = read_mpeg_frame_strict_into(&mut mss, &mut packet)?;
        let packet = &packet[..MPEG_HEADER_LEN + header.frame_size];
        let format_info = match header.layer {
            MpegLayer::Layer1 => &FormatInfos::MP1,
            MpegLayer::Layer2 => &FormatInfos::MP2,
            MpegLayer::Layer3 => &FormatInfos::MP3,
        };

        let mut codec_params = AudioCodecParameters::new();

        codec_params
            .for_codec(header.codec())
            .with_sample_rate(header.sample_rate)
            .with_channels(header.channel_mode.channels());

        let mut track = Track::new(0);

        track.with_codec_params(CodecParameters::Audio(codec_params));

        if let Some(info_tag) = try_read_info_tag(packet, &header) {
            if let Some(lame_tag) = info_tag.lame {
                track
                    .with_delay(lame_tag.enc_delay)
                    .with_padding(lame_tag.enc_padding);
            }

            if let Some(num_mpeg_frames) = info_tag.num_frames {
                info!("using xing header for duration");

                let num_frames = u64::from(num_mpeg_frames) * u64::from(header.num_frames());

                let discard = track.delay.unwrap_or(0) + track.padding.unwrap_or(0);

                track.with_num_frames(num_frames.saturating_sub(u64::from(discard)));
            }
        } else if let Some(vbri_tag) = try_read_vbri_tag(packet, &header) {
            info!("using vbri header for duration");

            let num_frames = u64::from(vbri_tag.num_mpeg_frames) * u64::from(header.num_frames());

            track.with_num_frames(num_frames);
        } else {
            mss.seek_buffered_rev(MPEG_HEADER_LEN + header.frame_size);

            if mss.is_seekable() {
                info!("estimating duration from bitrate, may be inaccurate for vbr files");

                if let Some(n_mpeg_frames) = estimate_num_mpeg_frames(&mut mss) {
                    track.with_num_frames(n_mpeg_frames * u64::from(header.num_frames()));
                }
            }
        }

        if let Some(num_frames) = track.num_frames {
            track.with_duration(Duration::from(num_frames));
        }

        let first_packet_pos = mss.pos();
        let next_packet_ts = Timestamp::from(-i64::from(track.delay.unwrap_or(0)));

        Ok(MpaReader {
            format_info,
            first_packet_pos,
            next_packet_ts,
            reader: mss,
            media_info: MediaInfo::from_track(&track),
            tracks: vec![track],
            chapters: opts.external_data.chapters,
            metadata: opts.external_data.metadata.unwrap_or_default(),
        })
    }
}

/// Rolls the reader back to `checkpoint` when `result` is a transient
/// (`Interrupted`/`WouldBlock`) I/O error so the caller can be retried from
/// the same frame boundary. Requires a seekback buffer covering the bytes
/// consumed since `checkpoint`.
fn roll_back_transient<T>(
    reader: &mut MediaSourceStream<'_>,
    checkpoint: u64,
    result: Result<T>,
) -> Result<T> {
    match result {
        Err(Error::IoError(err))
            if err.kind() == ErrorKind::Interrupted || err.kind() == ErrorKind::WouldBlock =>
        {
            if reader.seek_buffered(checkpoint) == checkpoint {
                Err(Error::IoError(err))
            } else {
                decode_error("mpa: failed to roll back transient seek-scan read")
            }
        }
        result => result,
    }
}

/// Reads an MPEG frame and returns the header and buffer.
fn read_mpeg_frame_into(
    reader: &mut MediaSourceStream<'_>,
    packet: &mut [u8],
) -> Result<FrameHeader> {
    let start = reader.pos();
    reader.ensure_seekback_buffer(MAX_MPEG_FRAME_SIZE);

    let result = read_mpeg_frame_inner(reader, packet);

    match result {
        Err(Error::IoError(err))
            if err.kind() == ErrorKind::Interrupted || err.kind() == ErrorKind::WouldBlock =>
        {
            if reader.seek_buffered(start) == start {
                Err(Error::IoError(err))
            } else {
                decode_error("mpa: failed to roll back transient frame read")
            }
        }
        result => result,
    }
}

fn read_mpeg_frame_inner(
    reader: &mut MediaSourceStream<'_>,
    packet: &mut [u8],
) -> Result<FrameHeader> {
    let (header, header_word) = loop {
        let sync = header::sync_frame(reader)?;

        if let Ok(header) = header::parse_frame_header(sync) {
            break (header, sync);
        }

        warn!("invalid mpeg audio header");
    };

    let packet_len = MPEG_HEADER_LEN + header.frame_size;
    if packet_len > packet.len() {
        return decode_error("mpa: MPEG frame exceeds the format maximum");
    }
    packet[0..MPEG_HEADER_LEN].copy_from_slice(&header_word.to_be_bytes());

    let mut body = &mut packet[MPEG_HEADER_LEN..packet_len];
    while !body.is_empty() {
        let read = reader.read_buf(body)?;
        body = &mut body[read..];
    }

    Ok(header)
}

/// Reads an MPEG frame and checks if the next frame begins after the packet.
///
/// Resumes scanning one byte into the rejected candidate, so the same position is not selected
/// again.
fn read_mpeg_frame_strict_into(
    reader: &mut MediaSourceStream<'_>,
    packet: &mut [u8],
) -> Result<FrameHeader> {
    loop {
        let header = read_mpeg_frame_into(reader, packet)?;
        let packet_len = MPEG_HEADER_LEN + header.frame_size;

        let pos = reader.pos();

        if let Ok(sync) = header::read_frame_header_word_no_sync(reader)
            && (!header::is_frame_header_word_synced(sync)
                || !is_frame_header_similar(&header, sync))
        {
            warn!("skipping junk at {} bytes", pos - packet_len as u64);

            reader.seek_buffered_rev(packet_len + MPEG_HEADER_LEN - 1);
            continue;
        }

        reader.seek_buffered(pos);

        break Ok(header);
    }
}

/// Check if a sync word parses to a frame header that is similar to the one provided.
fn is_frame_header_similar(header: &FrameHeader, sync: u32) -> bool {
    if let Ok(candidate) = header::parse_frame_header(sync)
        && header.version == candidate.version
        && header.layer == candidate.layer
        && header.sample_rate == candidate.sample_rate
        && header.n_channels() == candidate.n_channels()
    {
        return true;
    }

    false
}

#[derive(Default)]
struct FramePos {
    ts: Timestamp,
    pos: u64,
}

/// Reads the `main_data_begin` field from the side information of an MPEG audio frame.
fn read_main_data_begin<B: ReadBytes>(reader: &mut B, header: &FrameHeader) -> Result<u16> {
    const MPEG1_MAIN_DATA_SHIFT: u32 = 7;

    if header.has_crc {
        let _crc = reader.read_be_u16()?;
    }

    let main_data_begin = if header.is_mpeg1() {
        reader.read_be_u16()? >> MPEG1_MAIN_DATA_SHIFT
    } else {
        u16::from(reader.read_u8()?)
    };

    Ok(main_data_begin)
}

/// Estimates the total number of MPEG frames in the media source stream from the average length
/// of the frames at its start. Returns `None` when their bitrates differ: extrapolating a variable
/// bitrate publishes an end that can fall well before the last frame.
fn estimate_num_mpeg_frames(reader: &mut MediaSourceStream<'_>) -> Option<u64> {
    const MAX_FRAMES: u32 = 16;
    const MAX_LEN: usize = 16 * 1024;

    macro_rules! break_on_err {
        ($expr:expr) => {
            match $expr {
                Ok(a) => a,
                _ => break None,
            }
        };
    }

    let start_pos = reader.pos();

    let mut total_frame_len = 0;
    let mut total_frames = 0;
    let mut bitrate = None;

    let total_len = match reader.byte_len() {
        Some(len) => len - start_pos,
        _ => return None,
    };

    let num_mpeg_frames = loop {
        let header_val = break_on_err!(reader.read_be_u32());

        let header = break_on_err!(header::parse_frame_header(header_val));

        if *bitrate.get_or_insert(header.bitrate) != header.bitrate {
            break None;
        }

        total_frame_len += MPEG_HEADER_LEN + header.frame_size;
        total_frames += 1;

        break_on_err!(reader.ignore_bytes(header.frame_size as u64));

        if total_frames > MAX_FRAMES || total_frame_len > MAX_LEN {
            break total_frame_len.to_f64().zip(total_len.to_f64()).and_then(
                |(parsed_len, total_len)| {
                    let avg_mpeg_frame_len = parsed_len / f64::from(total_frames);
                    num_traits::NumCast::from(total_len / avg_mpeg_frame_len)
                },
            );
        }
    };

    reader.seek_buffered(start_pos);

    num_mpeg_frames
}
