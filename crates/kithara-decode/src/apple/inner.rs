//! `AppleInner` — decoder lifecycle: open container via the appropriate
//! `PacketReader`, feed compressed packets through `AudioConverter`, and
//! emit PCM chunks. The hot loop performs no per-packet allocations —
//! each packet is read as `&[u8]` borrowed from the reader's internal
//! buffer and handed to the converter via pointer + length.

#![allow(unsafe_code)]

use std::{
    cell::Cell,
    ffi::c_void,
    ptr,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    thread,
    time::Duration,
};

use kithara_bufpool::PcmPool;
use kithara_stream::ContainerFormat;
use tracing::{debug, trace, warn};

use super::{
    audiofile::AudioFileReader,
    config::AppleConfig,
    consts::{Consts, os_status_to_string},
    converter::{ConverterInputState, converter_input_callback},
    ffi::{
        AudioBuffer, AudioBufferList, AudioConverterDispose, AudioConverterFillComplexBuffer,
        AudioConverterNew, AudioConverterRef, AudioConverterReset, AudioConverterSetProperty,
        AudioStreamBasicDescription, UInt32,
    },
    fmp4::Fmp4Reader,
    reader::{PacketReader, container_to_file_type},
};
use crate::{
    backend::BoxedSource,
    error::{DecodeError, DecodeResult},
    types::{PcmChunk, PcmMeta, PcmSpec, TrackMetadata},
};

/// Resolve the source length, preferring the HTTP byte-length handle.
fn resolve_byte_len(source: &mut BoxedSource, config: &AppleConfig) -> Arc<AtomicU64> {
    if let Some(handle) = config.byte_len_handle.as_ref()
        && handle.load(Ordering::Acquire) > 0
    {
        return Arc::clone(handle);
    }
    let len = source
        .seek(std::io::SeekFrom::End(0))
        .ok()
        .and_then(|len| {
            source.seek(std::io::SeekFrom::Start(0)).ok()?;
            Some(len)
        })
        .unwrap_or(0);
    Arc::new(AtomicU64::new(len))
}

fn open_reader(
    source: BoxedSource,
    container: ContainerFormat,
    config: &AppleConfig,
    byte_len: Arc<AtomicU64>,
) -> DecodeResult<Box<dyn PacketReader>> {
    if container == ContainerFormat::Fmp4 {
        let len = byte_len.load(Ordering::Acquire);
        return Ok(Box::new(Fmp4Reader::open(
            source,
            len,
            config.byte_pool.as_ref(),
        )?));
    }
    let Some(file_type) = container_to_file_type(container) else {
        return Err(DecodeError::UnsupportedContainer(container));
    };
    Ok(Box::new(AudioFileReader::open(
        source, byte_len, file_type,
    )?))
}

pub(super) struct AppleInner {
    reader: Box<dyn PacketReader>,
    converter: AudioConverterRef,
    converter_input: Box<ConverterInputState>,
    pub(super) spec: PcmSpec,
    pub(super) position: Duration,
    frame_offset: u64,
    pub(super) duration: Option<Duration>,
    pub(super) metadata: TrackMetadata,
    pub(super) byte_len_handle: Arc<AtomicU64>,
    pcm_buffer: Vec<f32>,
    frames_per_packet: usize,
    eof: bool,
    frames_decoded: u64,
    pool: PcmPool,
    owner_thread: Cell<Option<thread::ThreadId>>,
}

impl AppleInner {
    #[inline]
    fn assert_thread_affinity(&self) {
        #[cfg(debug_assertions)]
        {
            let current = thread::current().id();
            if let Some(owner) = self.owner_thread.get() {
                debug_assert_eq!(
                    owner, current,
                    "Apple decoder used from multiple threads; this backend requires thread affinity"
                );
            } else {
                self.owner_thread.set(Some(current));
            }
        }
    }

    pub(super) fn try_new(
        mut source: BoxedSource,
        config: &AppleConfig,
    ) -> Result<Self, DecodeError> {
        let Some(container) = config.container else {
            return Err(DecodeError::InvalidData(
                "Container format must be specified for Apple decoder".into(),
            ));
        };

        let byte_len = resolve_byte_len(&mut source, config);
        debug!(
            ?container,
            byte_len = byte_len.load(Ordering::Acquire),
            "Apple decoder: opening container"
        );

        let reader = open_reader(source, container, config, Arc::clone(&byte_len))?;
        let format = reader.format();

        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "sample rate fits in u32 for valid audio"
        )]
        let sample_rate = format.mSampleRate as u32;
        #[expect(
            clippy::cast_possible_truncation,
            reason = "channel count fits in u16 for valid audio"
        )]
        let channels = format.mChannelsPerFrame as u16;

        let converter = Self::new_audio_converter(&format, channels)?;
        Self::apply_magic_cookie(converter, reader.magic_cookie());

        let spec = PcmSpec {
            channels,
            sample_rate,
        };
        let pool = config
            .pcm_pool
            .clone()
            .unwrap_or_else(|| kithara_bufpool::pcm_pool().clone());
        let frames_per_packet = if format.mFramesPerPacket > 0 {
            format.mFramesPerPacket as usize
        } else {
            Consts::DEFAULT_BUFFER_FRAMES
        };
        let pcm_buffer =
            vec![0.0f32; frames_per_packet.max(Consts::DEFAULT_BUFFER_FRAMES) * channels as usize];
        let duration = reader.duration();

        debug!(
            ?spec,
            ?duration,
            frames_per_packet,
            "Apple decoder: initialized"
        );

        Ok(Self {
            reader,
            converter,
            converter_input: Box::new(ConverterInputState::new()),
            spec,
            position: Duration::ZERO,
            frame_offset: 0,
            duration,
            metadata: TrackMetadata::default(),
            byte_len_handle: byte_len,
            pcm_buffer,
            frames_per_packet,
            eof: false,
            frames_decoded: 0,
            pool,
            owner_thread: Cell::new(None),
        })
    }

    fn new_audio_converter(
        input_format: &AudioStreamBasicDescription,
        channels: u16,
    ) -> DecodeResult<AudioConverterRef> {
        let output_format = AudioStreamBasicDescription {
            mSampleRate: input_format.mSampleRate,
            mFormatID: Consts::kAudioFormatLinearPCM,
            mFormatFlags: Consts::kAudioFormatFlagsNativeFloatPacked,
            mBytesPerPacket: Consts::BYTES_PER_F32_SAMPLE * u32::from(channels),
            mFramesPerPacket: 1,
            mBytesPerFrame: Consts::BYTES_PER_F32_SAMPLE * u32::from(channels),
            mChannelsPerFrame: u32::from(channels),
            mBitsPerChannel: Consts::BITS_PER_F32_SAMPLE,
            mReserved: 0,
        };

        let mut converter: AudioConverterRef = ptr::null_mut();
        // SAFETY: input/output formats are stack structs; `converter` is a
        // writable out-pointer.
        let status = unsafe { AudioConverterNew(input_format, &output_format, &mut converter) };
        if status != Consts::noErr {
            let err = os_status_to_string(status);
            warn!(status, err = %err, "AudioConverterNew failed");
            return Err(DecodeError::Backend(Box::new(std::io::Error::other(
                format!("AudioConverterNew failed: {err}"),
            ))));
        }
        Ok(converter)
    }

    fn apply_magic_cookie(converter: AudioConverterRef, cookie: Option<&[u8]>) {
        let Some(cookie) = cookie else { return };
        #[expect(
            clippy::cast_possible_truncation,
            reason = "magic cookie size fits in u32"
        )]
        // SAFETY: `converter` is a live handle, `cookie` is a readable byte slice.
        let status = unsafe {
            AudioConverterSetProperty(
                converter,
                Consts::kAudioConverterDecompressionMagicCookie,
                cookie.len() as UInt32,
                cookie.as_ptr() as *const c_void,
            )
        };
        if status == Consts::noErr {
            debug!(
                cookie_size = cookie.len(),
                "Apple decoder: magic cookie applied"
            );
        } else {
            warn!(
                status,
                err = %os_status_to_string(status),
                "Apple decoder: magic cookie apply failed (continuing)"
            );
        }
    }

    /// Refill the converter's input slot from the reader, returning
    /// `Ok(true)` if a packet was staged or `Ok(false)` on reader EOF.
    fn refill_input(&mut self) -> DecodeResult<bool> {
        if self.converter_input.has_packet {
            return Ok(true);
        }
        let Some(packet) = self.reader.read_next_packet()? else {
            tracing::info!("AppleInner: reader EOF");
            self.eof = true;
            return Ok(false);
        };
        tracing::info!(
            size = packet.data.len(),
            "AppleInner: got packet from reader"
        );
        self.converter_input.set(packet.data, packet.description);
        Ok(true)
    }

    pub(super) fn next_chunk(&mut self) -> DecodeResult<Option<PcmChunk>> {
        self.assert_thread_affinity();
        tracing::info!(eof = self.eof, "AppleInner::next_chunk called");
        if self.eof {
            return Ok(None);
        }
        loop {
            if !self.refill_input()? {
                return Ok(None);
            }

            let channels = self.spec.channels as usize;
            let output_frames = self.ensure_pcm_buffer_capacity(channels);

            if let Some(output_packets) = self.run_converter(output_frames)? {
                return Ok(Some(self.finalize_chunk(output_packets, channels)?));
            }
        }
    }

    fn ensure_pcm_buffer_capacity(&mut self, channels: usize) -> usize {
        let output_frames = self.frames_per_packet.max(Consts::DEFAULT_BUFFER_FRAMES);
        let needed = output_frames * channels;
        if self.pcm_buffer.len() < needed {
            self.pcm_buffer.resize(needed, 0.0);
        }
        output_frames
    }

    fn run_converter(&mut self, output_frames: usize) -> DecodeResult<Option<UInt32>> {
        #[expect(
            clippy::cast_possible_truncation,
            reason = "PCM buffer size fits in u32"
        )]
        let mut buffer_list = AudioBufferList {
            mNumberBuffers: 1,
            mBuffers: [AudioBuffer {
                mNumberChannels: u32::from(self.spec.channels),
                mDataByteSize: (self.pcm_buffer.len() * Consts::BYTES_PER_F32_SAMPLE as usize)
                    as u32,
                mData: self.pcm_buffer.as_mut_ptr() as *mut c_void,
            }],
        };

        #[expect(
            clippy::cast_possible_truncation,
            reason = "output frame count fits in u32"
        )]
        let mut output_packets = output_frames as UInt32;
        let input_ptr = self.converter_input.as_mut() as *mut ConverterInputState as *mut c_void;

        // SAFETY: `self.converter` is live; `input_ptr` points at a live
        // `ConverterInputState`; `buffer_list` is a valid output buffer.
        let status = unsafe {
            AudioConverterFillComplexBuffer(
                self.converter,
                converter_input_callback,
                input_ptr,
                &mut output_packets,
                &mut buffer_list,
                ptr::null_mut(),
            )
        };

        // NoDataNow with zero output → the callback ran out of input
        // before the converter produced any frames; loop back to fetch
        // more packets. NoDataNow with partial output → emit those
        // frames now (relevant for LinearPCM where one input packet
        // maps to one output frame but the converter still signals
        // NoDataNow for the unfilled tail of the buffer).
        if status == Consts::kAudioConverterErr_NoDataNow && output_packets == 0 {
            trace!("Apple decoder: converter needs more data");
            return Ok(None);
        }

        if status != Consts::noErr
            && status != Consts::kAudioConverterErr_NoDataNow
            && output_packets == 0
        {
            let err = os_status_to_string(status);
            warn!(status, err = %err, "AudioConverterFillComplexBuffer failed");
            return Err(DecodeError::Backend(Box::new(std::io::Error::other(
                format!("AudioConverterFillComplexBuffer failed: {err}"),
            ))));
        }

        if output_packets == 0 {
            return Ok(None);
        }

        Ok(Some(output_packets))
    }

    fn finalize_chunk(
        &mut self,
        output_packets: UInt32,
        channels: usize,
    ) -> DecodeResult<PcmChunk> {
        let frames = output_packets as usize;
        let samples = frames * channels;

        let mut pooled = self.pool.get();
        pooled
            .ensure_len(samples)
            .map_err(|e| DecodeError::Backend(Box::new(e)))?;
        pooled[..samples].copy_from_slice(&self.pcm_buffer[..samples]);

        let meta = PcmMeta {
            spec: self.spec,
            frame_offset: self.frame_offset,
            timestamp: self.position,
            segment_index: None,
            variant_index: None,
            epoch: 0,
        };
        let chunk = PcmChunk::new(meta, pooled);

        self.frames_decoded += frames as u64;
        self.frame_offset += frames as u64;
        #[expect(
            clippy::cast_precision_loss,
            reason = "precision loss acceptable for position tracking"
        )]
        let pos_secs = self.frames_decoded as f64 / f64::from(self.spec.sample_rate);
        self.position = Duration::from_secs_f64(pos_secs);

        trace!(
            frames,
            samples,
            position_ms = self.position.as_millis(),
            "Apple decoder: decoded chunk"
        );

        Ok(chunk)
    }

    pub(super) fn seek(&mut self, pos: Duration) -> DecodeResult<()> {
        self.assert_thread_affinity();

        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "target frame fits in u64 for realistic durations"
        )]
        let target_frame = (pos.as_secs_f64() * f64::from(self.spec.sample_rate)) as u64;

        let aligned_frame = self.reader.seek_to_frame(target_frame)?;

        // SAFETY: `self.converter` is a live handle.
        let status = unsafe { AudioConverterReset(self.converter) };
        if status != Consts::noErr {
            warn!(
                status,
                err = %os_status_to_string(status),
                "Apple decoder: converter reset after seek failed"
            );
        }

        self.converter_input.clear();
        self.eof = false;

        self.frames_decoded = aligned_frame;
        self.frame_offset = aligned_frame;
        #[expect(
            clippy::cast_precision_loss,
            reason = "precision loss acceptable for position tracking"
        )]
        let pos_secs = aligned_frame as f64 / f64::from(self.spec.sample_rate);
        self.position = Duration::from_secs_f64(pos_secs);

        debug!(
            target_frame,
            aligned_frame,
            new_position = ?self.position,
            "Apple decoder: seek complete"
        );

        Ok(())
    }
}

// SAFETY: AudioToolbox handles are only touched under thread-affinity
// assertions (`assert_thread_affinity`). Cross-thread access is limited
// to the byte-len atomic, which synchronizes on its own.
unsafe impl Send for AppleInner {}

impl Drop for AppleInner {
    fn drop(&mut self) {
        self.assert_thread_affinity();
        debug!("Apple decoder: disposing");
        // SAFETY: `self.converter` came from `AudioConverterNew` and is
        // disposed at most once.
        unsafe {
            if !self.converter.is_null() {
                AudioConverterDispose(self.converter);
            }
        }
    }
}
