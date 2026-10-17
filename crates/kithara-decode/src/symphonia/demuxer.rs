use std::{
    io::ErrorKind,
    num::NonZeroU16,
    sync::atomic::{AtomicU64, Ordering},
};

use kithara_platform::{sync::Arc, time::Duration};
use kithara_stream::{AudioCodec, PendingReason, PrerollHint, ReaderControl};
use kithara_test_utils::kithara;
#[cfg(feature = "symphonia")]
use symphonia_core::codecs::audio::AudioCodecParameters;
use symphonia_core::{
    codecs::CodecParameters,
    errors::Error as SymphoniaError,
    formats::{FormatReader, SeekMode, SeekTo, TrackType},
    units::{Duration as SymphoniaDuration, Time, TimeBase, Timestamp},
};

use super::{
    error::{classify_seek_err, pending_reason},
    packets::Packets,
    track::{build_track_info, time_to_duration, timestamp_origin},
};
use crate::{
    CodecPriming, DecodeError, DecodeResult, InputState, PrepareWait,
    demuxer::{DemuxOutcome, DemuxSeekOutcome, Demuxer, Frame, PreparedPacket, TrackInfo},
};

/// Demuxer adapter over Symphonia's [`FormatReader`].
pub(crate) struct SymphoniaDemuxer {
    /// Native Symphonia codec parameters for the audio track. Carried so
    /// the matching [`SymphoniaCodec::open_native`] path can build a
    /// decoder for codecs whose generic [`AudioCodec`] enum representation
    /// loses information (PCM bit-depth/endianness, ADPCM dialect).
    #[cfg(feature = "symphonia")]
    pub(crate) native_params: AudioCodecParameters,
    format_reader: Packets,
    prepared: Option<PreparedPacket>,
    byte_map: Option<Arc<dyn kithara_stream::ByteMap>>,
    /// Live byte cursor of the underlying media source. Populated by
    /// the [`super::super::symphonia::adapter::ReadSeekAdapter`] when
    /// the demuxer is built through that path; absent for synthetic
    /// readers in unit tests.
    byte_pos_handle: Option<Arc<AtomicU64>>,
    pending_seek: Option<CodecPriming>,
    /// Time base used to translate packet timestamps into wall-clock
    /// [`std::time::Duration`].
    time_base: TimeBase,
    /// Declared native origin translated to the public raw PCM basis.
    timestamp_origin: Timestamp,
    media_origin: Option<Duration>,
    track_info: TrackInfo,
    track_id: u32,
}

#[cfg(all(test, feature = "symphonia"))]
mod raw_clock_tests {
    use std::{collections::VecDeque, io::Cursor};

    use kithara_mpa::MpaReader;
    use kithara_platform::time::Duration;
    use kithara_test_fixtures::assets::signal_profile_mp3_libmp3lame_44100_1ch;
    use kithara_test_utils::kithara;
    use symphonia_core::{
        errors::Result as SymphoniaResult,
        formats::{
            FormatInfo, FormatOptions, FormatReader, MediaInfo, SeekMode, SeekTo, SeekedTo, Track,
            well_known::{FORMAT_ID_MKV, FORMAT_ID_OGG},
        },
        io::{MediaSourceStream, MediaSourceStreamOptions},
        meta::Metadata,
        packet::Packet,
        units::{Duration as PacketDuration, Timestamp},
    };

    use super::SymphoniaDemuxer;
    use crate::{
        CodecPriming, DecodeError,
        demuxer::{DemuxOutcome, DemuxSeekOutcome, Demuxer},
    };

    fn reader() -> MpaReader<'static> {
        let source = Cursor::new(signal_profile_mp3_libmp3lame_44100_1ch().bytes().to_vec());
        let stream = MediaSourceStream::new(Box::new(source), MediaSourceStreamOptions::default());
        MpaReader::try_new(stream, FormatOptions::default()).expect("MPEG fixture reader")
    }

    fn frame(demuxer: &mut SymphoniaDemuxer) -> (Duration, Duration, Vec<u8>) {
        match demuxer.next_frame().expect("fixture frame") {
            DemuxOutcome::Frame(frame) => (frame.pts, frame.duration, frame.data.to_vec()),
            _ => panic!("complete fixture must return a frame"),
        }
    }

    #[kithara::test]
    fn mpeg_head_and_positive_packets_keep_their_raw_pcm_positions() {
        let mut native = reader();
        let delay = native.tracks()[0].delay.expect("LAME delay");
        assert_eq!(delay, 1_105);
        let first = native
            .next_packet()
            .expect("first packet")
            .expect("first packet exists");
        let second = native
            .next_packet()
            .expect("second packet")
            .expect("second packet exists");
        assert_eq!(first.pts.get(), -i64::from(delay));
        assert_eq!(second.pts.get(), 47);
        let mut demuxer = SymphoniaDemuxer::from_reader_with_layout(
            Box::new(reader()),
            None,
            None,
            None,
            kithara_stream::ReaderControl::new(None),
        )
        .expect("MPEG demuxer");
        let head = frame(&mut demuxer);
        let next = frame(&mut demuxer);
        assert_eq!(head.0, Duration::ZERO);
        assert_eq!(next.0, Duration::from_nanos(26_122_448));
        assert_eq!(head.1, Duration::from_nanos(26_122_448));
        assert_eq!(next.1, head.1);
        assert_eq!(head.2, first.data.as_ref());
        assert_eq!(next.2, second.data.as_ref());
    }

    #[kithara::test]
    #[case::head(Duration::ZERO)]
    #[case::middle(Duration::from_millis(1_500))]
    #[case::end(Duration::from_millis(6_010))]
    fn mpeg_seek_translates_the_public_target_and_native_landing(#[case] target: Duration) {
        let mut native = reader();
        let delay = native.tracks()[0].delay.expect("LAME delay");
        let effective = target.saturating_sub(Duration::from_secs_f64(1_152.0 / 44_100.0));
        let target_tick = i64::try_from(effective.as_nanos() * 44_100 / 1_000_000_000)
            .expect("fixture target tick");
        let expected = native
            .seek(
                SeekMode::Accurate,
                SeekTo::Timestamp {
                    ts: Timestamp::new(target_tick - i64::from(delay)),
                    track_id: 0,
                },
            )
            .expect("native reference seek");
        let packet = native
            .next_packet()
            .expect("native sought packet")
            .expect("sought packet exists");
        let raw_tick =
            u64::try_from(expected.actual_ts.get() + i64::from(delay)).expect("native raw landing");
        let expected_position = Duration::from_nanos(raw_tick * 1_000_000_000 / 44_100);
        let mut demuxer = SymphoniaDemuxer::from_reader_with_layout(
            Box::new(reader()),
            None,
            None,
            None,
            kithara_stream::ReaderControl::new(None),
        )
        .expect("MPEG demuxer");
        let outcome = demuxer
            .seek(target, CodecPriming::default())
            .expect("public raw seek");
        let DemuxSeekOutcome::Landed { landed_at, .. } = outcome else {
            panic!("in-range fixture seek must land");
        };
        assert_eq!(landed_at, expected_position);
        let actual = frame(&mut demuxer);
        assert_eq!(actual.0, expected_position);
        assert_eq!(actual.2, packet.data.as_ref());
    }

    #[kithara::test]
    fn mpeg_seek_past_the_raw_end_remains_out_of_range() {
        let mut demuxer = SymphoniaDemuxer::from_reader_with_layout(
            Box::new(reader()),
            None,
            None,
            None,
            kithara_stream::ReaderControl::new(None),
        )
        .expect("MPEG demuxer");
        assert!(matches!(
            demuxer.seek(Duration::from_secs(7), CodecPriming::default()),
            Err(DecodeError::SeekOutOfRange { .. })
        ));
    }

    struct ScriptedReader {
        inner: Box<dyn FormatReader>,
        tracks: Vec<Track>,
        packets: VecDeque<Packet>,
        format: FormatInfo,
    }

    impl FormatReader for ScriptedReader {
        fn format_info(&self) -> &FormatInfo {
            &self.format
        }

        delegate::delegate! {
            to self.inner {
                fn media_info(&self) -> &MediaInfo;
                fn metadata(&mut self) -> Metadata<'_>;
                fn seek(&mut self, mode: SeekMode, to: SeekTo) -> SymphoniaResult<SeekedTo>;
                fn into_inner<'s>(self: Box<Self>) -> MediaSourceStream<'s>
                where
                    Self: 's,;
            }
        }

        fn tracks(&self) -> &[Track] {
            &self.tracks
        }
        fn next_packet(&mut self) -> SymphoniaResult<Option<Packet>> {
            Ok(self.packets.pop_front())
        }
    }

    fn scripted(format: FormatInfo, timestamps: &[i64]) -> SymphoniaDemuxer {
        let packets = timestamps
            .iter()
            .map(|&timestamp| {
                Packet::new(
                    0,
                    Timestamp::new(timestamp),
                    PacketDuration::new(882),
                    vec![1],
                )
            })
            .collect();
        let inner = reader();
        let tracks = inner.tracks().to_vec();
        SymphoniaDemuxer::from_reader_with_layout(
            Box::new(ScriptedReader {
                inner: Box::new(inner),
                tracks,
                packets,
                format,
            }),
            None,
            None,
            None,
            kithara_stream::ReaderControl::new(None),
        )
        .expect("scripted demuxer")
    }

    #[kithara::test]
    fn non_mpeg_mp3_keeps_its_nonzero_start_and_real_pts_gap() {
        let mut format = *reader().format_info();
        format.format = FORMAT_ID_MKV;
        let mut demuxer = scripted(format, &[30_870, 44_100]);
        let first = frame(&mut demuxer);
        let second = frame(&mut demuxer);
        assert_eq!(first.0, Duration::from_millis(700));
        assert_eq!(second.0, Duration::from_secs(1));
        assert_eq!(first.1, Duration::from_millis(20));
        assert_eq!(second.0 - first.0 - first.1, Duration::from_millis(280));
    }

    #[kithara::test]
    fn a_packet_before_its_declared_origin_returns_invalid_data() {
        let format = *reader().format_info();
        let mut demuxer = scripted(format, &[-1_106]);
        assert!(matches!(
            demuxer.next_frame(),
            Err(DecodeError::InvalidData { .. })
        ));
    }

    #[kithara::test]
    fn positive_ogg_start_preserves_absolute_pts_and_real_gap() {
        let bytes = kithara_test_fixtures::assets::signal_profile_ogg_vorbis_44100_2ch().bytes();
        let stream = MediaSourceStream::new(
            Box::new(Cursor::new(bytes.to_vec())),
            MediaSourceStreamOptions::default(),
        );
        let inner =
            symphonia::default::formats::OggReader::try_new(stream, FormatOptions::default())
                .expect("Ogg metadata reader");
        let format = *inner.format_info();
        assert_eq!(format.format, FORMAT_ID_OGG);
        let mut tracks = inner.tracks().to_vec();
        let track = tracks.first_mut().expect("Ogg audio track");
        let track_id = track.id;
        track.start_ts = Timestamp::new(30_870);
        track.num_frames = Some(44_982);
        track.padding = Some(576);
        let packets = [30_870, 44_100]
            .into_iter()
            .map(|pts| {
                Packet::new(
                    track_id,
                    Timestamp::new(pts),
                    PacketDuration::new(882),
                    vec![1],
                )
            })
            .collect();
        let mut demuxer = SymphoniaDemuxer::from_reader_with_layout(
            Box::new(ScriptedReader {
                inner: Box::new(inner),
                tracks,
                packets,
                format,
            }),
            None,
            None,
            None,
            kithara_stream::ReaderControl::new(None),
        )
        .expect("positive-origin Ogg demuxer");
        let first = frame(&mut demuxer);
        let second = frame(&mut demuxer);
        assert_eq!(first.0, Duration::from_millis(700));
        assert_eq!(second.0, Duration::from_secs(1));
        assert_eq!(second.0 - first.0 - first.1, Duration::from_millis(280));
        assert_eq!(
            demuxer.duration(),
            Some(crate::DecoderDuration::RawLength(Duration::from_nanos(
                333_061_224
            )))
        );
    }
}

#[cfg(all(test, feature = "symphonia"))]
mod ogg_raw_clock_tests {
    use std::io::Cursor;

    use kithara_platform::time::Duration;
    use kithara_stream::{AudioCodec, ContainerFormat, MediaInfo};
    use kithara_test_fixtures::assets::{
        signal_profile_ogg_vorbis_44100_2ch, signal_profile_opus_libopus_48000_2ch,
    };
    use kithara_test_utils::kithara;
    use symphonia::default::formats::OggReader;
    use symphonia_core::{
        formats::{FormatOptions, FormatReader, SeekMode, SeekTo, TrackType},
        io::{MediaSourceStream, MediaSourceStreamOptions},
        units::Timestamp,
    };

    use super::SymphoniaDemuxer;
    use crate::{
        CodecPriming, DecoderBackend, DecoderChunkOutcome, DecoderConfig, DecoderFactory,
        PcmOrigin,
        demuxer::{DemuxOutcome, DemuxSeekOutcome, Demuxer},
        test_pools::{TestPools, pools},
    };

    fn reader(bytes: &'static [u8]) -> OggReader<'static> {
        let stream = MediaSourceStream::new(
            Box::new(Cursor::new(bytes.to_vec())),
            MediaSourceStreamOptions::default(),
        );
        OggReader::try_new(stream, FormatOptions::default()).expect("real Ogg reader")
    }

    fn duration(frames: u64, rate: u32) -> Duration {
        Duration::from_nanos(frames * 1_000_000_000 / u64::from(rate))
    }

    #[kithara::test]
    #[case::vorbis(signal_profile_ogg_vorbis_44100_2ch().bytes(), 44_100, -1_024, 1_024, 576, 264_640, 266_240)]
    #[case::opus(signal_profile_opus_libopus_48000_2ch().bytes(), 48_000, 0, 312, 648, 288_312, 288_960)]
    #[kithara::hang_watchdog]
    fn ogg_declared_origin_and_raw_extent_match_real_packet_geometry(
        #[case] bytes: &'static [u8],
        #[case] rate: u32,
        #[case] origin: i64,
        #[case] delay: u32,
        #[case] padding: u32,
        #[case] native_end: u64,
        #[case] raw_extent: u64,
    ) {
        let mut native = reader(bytes);
        let track = native
            .default_track(TrackType::Audio)
            .expect("Ogg audio track")
            .clone();
        assert_eq!(track.start_ts.get(), origin);
        assert_eq!(track.delay, Some(delay));
        assert_eq!(track.padding, Some(padding));
        assert_eq!(track.num_frames, Some(native_end));
        let mut demuxer = SymphoniaDemuxer::from_reader_with_layout(
            Box::new(reader(bytes)),
            None,
            None,
            None,
            kithara_stream::ReaderControl::new(None),
        )
        .expect("Ogg demuxer");
        let mut coded_frames = 0u64;
        let mut index = 0;
        while let Some(packet) = native.next_packet().expect("Ogg packet") {
            hang_tick!();
            let expected_raw = u64::try_from(packet.pts.get() - origin).expect("raw packet point");
            coded_frames += packet.dur.get() + packet.trim_start.get() + packet.trim_end.get();
            let outcome = demuxer.next_frame();
            let frame = match outcome {
                Ok(DemuxOutcome::Frame(frame)) => frame,
                other => panic!(
                    "Ogg raw frame index={index} native_pts={} origin={origin} delay={delay} \
                     padding={padding} native_end={native_end} raw_extent={raw_extent}: {other:?}",
                    packet.pts.get(),
                ),
            };
            assert_eq!(frame.pts, duration(expected_raw, rate));
            assert_eq!(frame.duration, duration(packet.dur.get(), rate));
            assert_eq!(frame.data, packet.data.as_ref());
            hang_reset!();
            index += 1;
        }
        assert!(matches!(
            demuxer.next_frame().expect("Ogg EOF"),
            DemuxOutcome::Eof
        ));
        assert_eq!(coded_frames, raw_extent);
        assert_eq!(
            demuxer.duration(),
            Some(crate::DecoderDuration::RawLength(duration(
                raw_extent, rate
            )))
        );
    }

    #[kithara::test]
    #[case::vorbis_zero(signal_profile_ogg_vorbis_44100_2ch().bytes(), 44_100, -1_024, Duration::ZERO)]
    #[case::vorbis_middle(signal_profile_ogg_vorbis_44100_2ch().bytes(), 44_100, -1_024, Duration::from_millis(1_500))]
    #[case::vorbis_end(signal_profile_ogg_vorbis_44100_2ch().bytes(), 44_100, -1_024, Duration::from_millis(5_900))]
    #[case::opus_zero(signal_profile_opus_libopus_48000_2ch().bytes(), 48_000, 0, Duration::ZERO)]
    #[case::opus_middle(signal_profile_opus_libopus_48000_2ch().bytes(), 48_000, 0, Duration::from_millis(1_500))]
    #[case::opus_end(signal_profile_opus_libopus_48000_2ch().bytes(), 48_000, 0, Duration::from_millis(5_900))]
    fn ogg_raw_seek_preserves_native_landing_and_packet(
        #[case] bytes: &'static [u8],
        #[case] rate: u32,
        #[case] origin: i64,
        #[case] target: Duration,
    ) {
        let mut native = reader(bytes);
        let track_id = native
            .default_track(TrackType::Audio)
            .expect("Ogg audio track")
            .id;
        let target_tick = i64::try_from(target.as_nanos() * u128::from(rate) / 1_000_000_000)
            .expect("Ogg seek target");
        let expected = native
            .seek(
                SeekMode::Accurate,
                SeekTo::Timestamp {
                    ts: Timestamp::new(target_tick + origin),
                    track_id,
                },
            )
            .expect("native Ogg seek");
        let packet = native
            .next_packet()
            .expect("sought Ogg packet")
            .expect("sought packet exists");
        let raw_landing = u64::try_from(expected.actual_ts.get() - origin).expect("raw landing");
        let mut demuxer = SymphoniaDemuxer::from_reader_with_layout(
            Box::new(reader(bytes)),
            None,
            None,
            None,
            kithara_stream::ReaderControl::new(None),
        )
        .expect("Ogg demuxer");
        let outcome = demuxer
            .seek(target, CodecPriming::default())
            .expect("raw Ogg seek");
        let DemuxSeekOutcome::Landed { landed_at, .. } = outcome else {
            panic!("supported Ogg seek must land");
        };
        assert_eq!(landed_at, duration(raw_landing, rate));
        let DemuxOutcome::Frame(frame) = demuxer.next_frame().expect("sought raw Ogg frame") else {
            panic!("sought Ogg packet must remain readable");
        };
        let raw_packet = u64::try_from(packet.pts.get() - origin).expect("sought raw packet");
        assert_eq!(frame.pts, duration(raw_packet, rate));
        assert_eq!(frame.data, packet.data.as_ref());
    }

    #[kithara::hang_watchdog]
    fn assert_pcm_geometry(
        bytes: &'static [u8],
        codec: AudioCodec,
        rate: u32,
        raw_extent: u64,
        first_frame: u64,
        emitted_frames: u64,
    ) {
        let config = DecoderConfig::<kithara_resampler::NoResamplerBackend, TestPools>::builder()
            .backend(DecoderBackend::Symphonia)
            .gapless(true)
            .pools(pools())
            .build();
        let media = MediaInfo::builder()
            .codec(codec)
            .container(ContainerFormat::Ogg)
            .build();
        let mut decoder =
            DecoderFactory::create_from_media_info(Cursor::new(bytes.to_vec()), &media, config)
                .expect("real Ogg codec");
        assert_eq!(
            decoder.duration(),
            Some(crate::DecoderDuration::RawLength(duration(
                raw_extent, rate
            )))
        );
        let mut frames = 0u64;
        loop {
            hang_tick!();
            match decoder.next_chunk().expect("real Ogg PCM") {
                DecoderChunkOutcome::Chunk(chunk) => {
                    let expected_frame = first_frame + frames;
                    assert_eq!(chunk.meta.frame_offset, expected_frame);
                    assert!(
                        chunk
                            .meta
                            .timestamp
                            .abs_diff(duration(expected_frame, rate))
                            <= Duration::from_nanos(1)
                    );
                    assert_eq!(
                        chunk.samples.len(),
                        chunk.frames() * usize::from(chunk.meta.spec.channels)
                    );
                    assert!(chunk.samples.iter().all(|sample| sample.is_finite()));
                    frames += u64::from(chunk.meta.frames);
                    hang_reset!();
                }
                DecoderChunkOutcome::Eof => break,
                DecoderChunkOutcome::Pending(reason) => {
                    panic!("complete Ogg PCM is pending: {reason:?}")
                }
            }
        }
        assert_eq!(
            decoder.initial_pcm_origin().map(PcmOrigin::position),
            Some(duration(first_frame, rate))
        );
        assert_eq!(frames, emitted_frames);
        assert_eq!(first_frame + frames, raw_extent);
    }

    #[kithara::test]
    fn vorbis_overlap_stays_with_the_codec_on_the_declared_raw_clock() {
        assert_pcm_geometry(
            signal_profile_ogg_vorbis_44100_2ch().bytes(),
            AudioCodec::Vorbis,
            44_100,
            266_240,
            1_024,
            265_216,
        );
    }

    #[cfg(feature = "opus")]
    #[kithara::test]
    fn opus_pre_skip_stays_outside_the_codec_on_the_declared_raw_clock() {
        assert_pcm_geometry(
            signal_profile_opus_libopus_48000_2ch().bytes(),
            AudioCodec::Opus,
            48_000,
            288_960,
            0,
            288_960,
        );
    }
}

impl SymphoniaDemuxer {
    fn current_byte(&self) -> Option<u64> {
        self.byte_pos_handle
            .as_ref()
            .map(|h| h.load(Ordering::Acquire))
    }

    fn dur_to_duration(&self, dur: SymphoniaDuration) -> DecodeResult<Duration> {
        self.time_base
            .calc_duration(dur)
            .and_then(time_to_duration)
            .ok_or(DecodeError::InvalidData {
                detail: "packet duration is outside the public time range",
            })
    }

    /// Build from an already-constructed `FormatReader`.
    ///
    /// The factory layer is responsible for wiring up the
    /// `Source -> MediaSource` adapter and probing the right reader
    /// for the container; `SymphoniaDemuxer` only deals with the
    /// post-bootstrap object.
    ///
    /// # Errors
    ///
    /// Returns a [`crate::DecodeError`] when the reader exposes no
    /// audio track or the audio track's codec parameters are missing
    /// fields the demuxer needs (sample rate, channel count).
    pub(crate) fn from_reader_with_layout(
        format_reader: Box<dyn FormatReader>,
        byte_pos_handle: Option<Arc<AtomicU64>>,
        byte_map: Option<Arc<dyn kithara_stream::ByteMap>>,
        wav_block_align: Option<NonZeroU16>,
        control: ReaderControl,
    ) -> DecodeResult<Self> {
        let track = format_reader
            .default_track(TrackType::Audio)
            .ok_or(DecodeError::ProbeFailed)?
            .clone();
        let track_id = track.id;
        let Some(CodecParameters::Audio(native_params)) = &track.codec_params else {
            return Err(DecodeError::ProbeFailed);
        };
        let native_params = native_params.clone();
        let format = format_reader.format_info().format;
        let track_info = build_track_info(&track, &native_params, format)?;
        let time_base = track.time_base.ok_or(DecodeError::InvalidData {
            detail: "audio track has no time base",
        })?;
        let timestamp_origin = timestamp_origin(&track, format);
        let media_origin = Timestamp::ZERO
            .duration_from(timestamp_origin)
            .and_then(|duration| time_base.calc_duration(duration))
            .and_then(time_to_duration);
        let format_reader = Packets::new(
            format_reader,
            track_id,
            &native_params,
            wav_block_align,
            control,
        )?;
        Ok(Self {
            track_id,
            track_info,
            #[cfg(feature = "symphonia")]
            native_params,
            time_base,
            timestamp_origin,
            media_origin,
            byte_pos_handle,
            byte_map,
            format_reader,
            pending_seek: None,
            prepared: None,
        })
    }

    fn ts_to_duration(&self, ts: Timestamp) -> DecodeResult<Duration> {
        let duration = ts
            .duration_from(self.timestamp_origin)
            .ok_or(DecodeError::InvalidData {
                detail: "packet timestamp precedes its raw PCM origin",
            })?;
        self.dur_to_duration(duration)
    }

    fn duration_to_native_timestamp(&self, duration: Duration) -> DecodeResult<Timestamp> {
        let seconds =
            i64::try_from(duration.as_secs()).map_err(|_| DecodeError::SeekOutOfRange {
                detail: "seek target is outside the native time range",
            })?;
        let time =
            Time::try_new(seconds, duration.subsec_nanos()).ok_or(DecodeError::SeekOutOfRange {
                detail: "seek target is outside the native time range",
            })?;
        let timestamp = self
            .time_base
            .calc_timestamp(time)
            .and_then(|timestamp| timestamp.get().checked_add(self.timestamp_origin.get()))
            .ok_or(DecodeError::SeekOutOfRange {
                detail: "seek timestamp overflows the native time range",
            })?;
        Ok(Timestamp::new(timestamp))
    }
}

impl Demuxer for SymphoniaDemuxer {
    fn current_segment_index(&self) -> Option<u32> {
        let byte = self.current_byte()?;
        self.byte_map
            .as_ref()?
            .segment_at_byte(byte.saturating_sub(1))
            .map(|d| d.segment_index)
    }

    fn current_variant_index(&self) -> Option<usize> {
        let byte = self.current_byte()?;
        self.byte_map
            .as_ref()?
            .segment_at_byte(byte.saturating_sub(1))
            .map(|d| d.variant_index)
    }

    fn duration(&self) -> Option<crate::DecoderDuration> {
        self.track_info.duration
    }

    fn next_frame(&mut self) -> DecodeResult<DemuxOutcome<'_>> {
        self.prepare_frame()?;
        self.next_frame_prepared()
    }

    fn next_frame_prepared(&mut self) -> DecodeResult<DemuxOutcome<'_>> {
        Ok(match self.prepared.take() {
            Some(PreparedPacket::Frame { pts, duration }) => DemuxOutcome::Frame(Frame {
                pts,
                duration,
                data: self.format_reader.data(),
                packet_desc: &[],
            }),
            Some(PreparedPacket::Pending(reason)) => DemuxOutcome::Pending(reason),
            Some(PreparedPacket::Eof) => {
                self.prepared = Some(PreparedPacket::Eof);
                DemuxOutcome::Eof
            }
            None => DemuxOutcome::Pending(PendingReason::Retry),
        })
    }

    fn prepare_frame(&mut self) -> DecodeResult<()> {
        self.prepare_frame_with_wait(PrepareWait::Wait)
    }

    fn prepare_frame_with_wait(&mut self, wait: PrepareWait) -> DecodeResult<()> {
        if !matches!(
            self.prepared,
            Some(PreparedPacket::Frame { .. } | PreparedPacket::Eof)
        ) {
            self.prepared = Some(self.read_frame(wait)?.into());
        }
        Ok(())
    }

    fn release_input(&mut self) -> InputState {
        self.prepared = None;
        self.pending_seek = None;
        self.format_reader.release_input()
    }

    fn seek(&mut self, target: Duration, priming: CodecPriming) -> DecodeResult<DemuxSeekOutcome> {
        self.seek_with_wait(target, priming, PrepareWait::Wait)
    }

    fn seek_with_wait(
        &mut self,
        target: Duration,
        priming: CodecPriming,
        wait: PrepareWait,
    ) -> DecodeResult<DemuxSeekOutcome> {
        self.prepared = None;
        let sr = f64::from(self.track_info.sample_rate.max(1));
        let priming_secs = f64::from(u32::try_from(priming.frames).unwrap_or(u32::MAX)) / sr;
        let packet_secs = f64::from(mdct_packet_frames(self.track_info.codec)) / sr;
        let backup_duration = Duration::from_secs_f64(priming_secs.max(packet_secs));
        let effective_target = target.saturating_sub(backup_duration);
        let seek_to = SeekTo::Timestamp {
            ts: self.duration_to_native_timestamp(effective_target)?,
            track_id: self.track_id,
        };
        self.pending_seek = Some(priming);
        match self
            .format_reader
            .seek(SeekMode::Accurate, seek_to, wait)
            .map_err(|e| classify_seek_err(&e))?
        {
            Some(seeked) => {
                self.pending_seek = None;
                self.settle_seek(seeked, priming)
            }
            None => Ok(DemuxSeekOutcome::Pending(
                self.format_reader.pending_reason(),
            )),
        }
    }

    fn prepare_seek(&mut self) -> DecodeResult<Option<DemuxSeekOutcome>> {
        self.prepare_seek_with_wait(PrepareWait::Wait)
    }

    fn prepare_seek_with_wait(
        &mut self,
        wait: PrepareWait,
    ) -> DecodeResult<Option<DemuxSeekOutcome>> {
        let Some(priming) = self.pending_seek else {
            return Ok(None);
        };
        match self
            .format_reader
            .prepare_seek(wait)
            .map_err(|e| classify_seek_err(&e))?
        {
            Some(seeked) => {
                self.pending_seek = None;
                self.settle_seek(seeked, priming).map(Some)
            }
            None => Ok(Some(DemuxSeekOutcome::Pending(
                self.format_reader.pending_reason(),
            ))),
        }
    }

    fn media_origin(&self) -> Option<Duration> {
        self.media_origin
    }

    fn track_info(&self) -> &TrackInfo {
        &self.track_info
    }
}

impl SymphoniaDemuxer {
    fn settle_seek(
        &mut self,
        seeked: symphonia_core::formats::SeekedTo,
        priming: CodecPriming,
    ) -> DecodeResult<DemuxSeekOutcome> {
        let landed_at = self.ts_to_duration(seeked.actual_ts)?;
        if let Some(crate::DecoderDuration::RawLength(duration)) = self.track_info.duration
            && landed_at >= duration
        {
            return Ok(DemuxSeekOutcome::PastEof { duration });
        }
        let landed_byte = self.current_byte();
        let preroll = match landed_byte {
            Some(lb) if priming.byte_margin > 0 => {
                PrerollHint::Required(lb.saturating_sub(priming.byte_margin))
            }
            _ => PrerollHint::NotNeeded,
        };
        Ok(DemuxSeekOutcome::Landed {
            landed_at,
            landed_byte,
            preroll,
        })
    }

    #[kithara::probe]
    #[kithara::measure(label = "decode.symphonia.demux")]
    fn read_frame(&mut self, wait: PrepareWait) -> DecodeResult<DemuxOutcome<'_>> {
        loop {
            let packet = match self.format_reader.read(wait) {
                Ok(Some(packet)) => packet,
                Ok(None) => return Ok(DemuxOutcome::Eof),
                Err(SymphoniaError::IoError(error)) if error.kind() == ErrorKind::UnexpectedEof => {
                    return Ok(DemuxOutcome::Eof);
                }
                Err(error) => {
                    return pending_reason(&error).map_or_else(
                        || Err(DecodeError::backend(error)),
                        |reason| Ok(DemuxOutcome::Pending(reason)),
                    );
                }
            };
            if packet.track_id != self.track_id {
                continue;
            }
            let pts = self.ts_to_duration(packet.pts)?;
            let duration = self.dur_to_duration(packet.dur)?;
            return Ok(DemuxOutcome::Frame(Frame {
                data: self.format_reader.data(),
                duration,
                pts,
                packet_desc: &[],
            }));
        }
    }

    /// Set encoder trim obtained from container metadata.
    pub(crate) const fn set_gapless(&mut self, gapless: Option<crate::GaplessInfo>) {
        self.track_info.gapless = gapless;
    }

    #[cfg(feature = "symphonia")]
    pub(crate) fn set_mp4_presentations(
        &mut self,
        presentations: Vec<crate::mp4::Presentation>,
        gapless: bool,
    ) {
        if let Some(presentation) = presentations
            .into_iter()
            .find(|item| item.track_id == self.track_id)
        {
            self.media_origin = presentation.media_origin;
            if gapless {
                self.track_info.gapless = presentation.gapless;
            }
            if let Some(end) = presentation.media_end {
                self.track_info.duration = Some(crate::DecoderDuration::MediaEnd(end));
            }
        }
    }
}

const fn mdct_packet_frames(codec: AudioCodec) -> u32 {
    match codec {
        AudioCodec::Mp3 => 1152,
        AudioCodec::AacLc | AudioCodec::AacHe | AudioCodec::AacHeV2 => 1024,
        _ => 0,
    }
}

#[cfg(all(test, feature = "symphonia"))]
mod tests {
    use std::io::{self, ErrorKind, Read, Seek, SeekFrom};

    use kithara_stream::{NotReadyCause, PendingReason, StreamSeekPastEof};
    use kithara_test_fixtures::fixtures::tone_wav;
    use kithara_test_utils::kithara;
    use symphonia::{
        core::{
            errors::Error as SymphoniaError,
            formats::{FormatOptions, probe::Hint},
            io::{MediaSource, MediaSourceStream, MediaSourceStreamOptions},
            meta::MetadataOptions,
        },
        default,
    };

    use super::{DemuxOutcome, Demuxer, SymphoniaDemuxer, pending_reason};

    /// Source modelling a reader that publishes an exact length and refuses
    /// any seek beyond it, the way the HLS session reader does once every
    /// segment size is known.
    struct PublishedEndSource {
        bytes: Vec<u8>,
        pos: u64,
    }

    impl PublishedEndSource {
        fn new(bytes: Vec<u8>) -> Self {
            Self { bytes, pos: 0 }
        }

        fn len(&self) -> u64 {
            u64::try_from(self.bytes.len()).unwrap_or(u64::MAX)
        }
    }

    impl Read for PublishedEndSource {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let start = usize::try_from(self.pos)
                .unwrap_or(usize::MAX)
                .min(self.bytes.len());
            let end = start.saturating_add(buf.len()).min(self.bytes.len());
            buf[..end - start].copy_from_slice(&self.bytes[start..end]);
            self.pos = u64::try_from(end).unwrap_or(u64::MAX);
            Ok(end - start)
        }
    }

    impl Seek for PublishedEndSource {
        fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
            let len = self.len();
            let target = match pos {
                SeekFrom::Start(offset) => i128::from(offset),
                SeekFrom::Current(delta) => i128::from(self.pos).saturating_add(i128::from(delta)),
                SeekFrom::End(delta) => i128::from(len).saturating_add(i128::from(delta)),
            };
            let target = u64::try_from(target).unwrap_or(u64::MAX);
            if target > len {
                return Err(io::Error::new(
                    ErrorKind::InvalidInput,
                    StreamSeekPastEof::new(self.pos, len, target),
                ));
            }
            self.pos = target;
            Ok(target)
        }
    }

    impl MediaSource for PublishedEndSource {
        fn byte_len(&self) -> Option<u64> {
            Some(self.len())
        }

        fn is_seekable(&self) -> bool {
            true
        }
    }

    fn wav_demuxer(tone_wav: &[u8]) -> SymphoniaDemuxer {
        /// Tail withheld from the WAV fixture so the source publishes a length
        /// shorter than the header's frame count, the way a variant publishes
        /// only the segments whose sizes are already exact.
        const WITHHELD_TAIL_BYTES: usize = 4 * 1024;

        let mut bytes = tone_wav.to_vec();
        bytes.truncate(bytes.len() - WITHHELD_TAIL_BYTES);
        let source = PublishedEndSource::new(bytes);
        let stream = MediaSourceStream::new(Box::new(source), MediaSourceStreamOptions::default());
        let mut hint = Hint::new();
        hint.with_extension("wav");
        let format_reader = default::get_probe()
            .probe(
                &hint,
                stream,
                FormatOptions::default(),
                MetadataOptions::default(),
            )
            .expect("WAV fixture must probe");
        SymphoniaDemuxer::from_reader_with_layout(
            format_reader,
            None,
            None,
            None,
            kithara_stream::ReaderControl::new(None),
        )
        .expect("WAV demuxer must build")
    }

    #[kithara::test]
    fn physical_eof_precedes_a_truncated_wav_declared_extent(tone_wav: &'static [u8]) {
        let mut demuxer = wav_demuxer(tone_wav);
        let declared = demuxer.duration().expect("declared WAV extent");
        let mut end = kithara_platform::time::Duration::ZERO;
        loop {
            match demuxer.next_frame().expect("physical WAV packet") {
                DemuxOutcome::Frame(frame) => end = frame.pts + frame.duration,
                DemuxOutcome::Eof => break,
                DemuxOutcome::Pending(reason) => panic!("resident WAV is pending: {reason:?}"),
            }
        }
        let crate::DecoderDuration::RawLength(declared) = declared else {
            panic!("WAV raw extent");
        };
        assert!(end > kithara_platform::time::Duration::ZERO && end < declared);
        assert!(matches!(
            demuxer.next_frame().expect("settled physical EOF"),
            DemuxOutcome::Eof
        ));
    }

    #[kithara::test(native, flash(false))]
    fn would_block_uses_source_pending_reason() {
        let error = SymphoniaError::IoError(io::Error::from(ErrorKind::WouldBlock));

        assert_eq!(
            pending_reason(&error),
            Some(PendingReason::NotReady(NotReadyCause::SourcePending))
        );
    }
}
