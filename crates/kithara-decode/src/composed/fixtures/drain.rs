use std::sync::atomic::{AtomicU32, Ordering};

use kithara_bufpool::SampleBuffer;
use kithara_platform::sync::Arc;
use kithara_signal::AudioSpec;

use crate::{codec::FrameCodec, error::DecodeResult};

pub(in crate::composed) struct EofDrainCodec {
    pub(in crate::composed) empty_decode_calls: Arc<AtomicU32>,
    spec: AudioSpec,
    pcm: Vec<f32>,
    tail_pending: bool,
    frames_per_call: u32,
    tail_frames: u32,
}

impl EofDrainCodec {
    pub(in crate::composed) fn new(
        pcm: Vec<f32>,
        spec: AudioSpec,
        frames_per_call: u32,
        tail_frames: u32,
    ) -> Self {
        Self {
            pcm,
            empty_decode_calls: Arc::new(AtomicU32::new(0)),
            spec,
            frames_per_call,
            tail_frames,
            tail_pending: tail_frames > 0,
        }
    }
}

impl FrameCodec for EofDrainCodec {
    fn decode_frame(
        &mut self,
        frame: crate::demuxer::Frame<'_>,
        metadata: crate::demuxer::PacketMetadata,
        out: &mut SampleBuffer,
    ) -> DecodeResult<crate::codec::DecodedPacket> {
        let crate::demuxer::Frame {
            data: bytes,
            pts: _pts,
            packet_desc: _packet_desc,
            ..
        } = frame;
        let frames = if bytes.is_empty() {
            self.empty_decode_calls.fetch_add(1, Ordering::SeqCst);
            if self.tail_pending {
                self.tail_pending = false;
                self.tail_frames
            } else {
                0
            }
        } else {
            self.frames_per_call
        };
        super::write_silent_test_frame(&self.pcm, self.spec, frames, out)
            .map(|frames| crate::codec::DecodedPacket::new(frames, metadata))
    }

    fn flush(&mut self) -> DecodeResult<()> {
        self.tail_pending = self.tail_frames > 0;
        Ok(())
    }

    fn spec(&self) -> AudioSpec {
        self.spec
    }
}

pub(in crate::composed) struct QueueCodec {
    pub(in crate::composed) empty_decode_calls: Arc<AtomicU32>,
    spec: AudioSpec,
    pcm: Vec<f32>,
    tail_pending: bool,
    tail_frames: u32,
}

impl QueueCodec {
    pub(in crate::composed) fn new(pcm: Vec<f32>, spec: AudioSpec, tail_frames: u32) -> Self {
        Self {
            pcm,
            spec,
            tail_frames,
            empty_decode_calls: Arc::new(AtomicU32::new(0)),
            tail_pending: false,
        }
    }
}

impl FrameCodec for QueueCodec {
    fn decode_frame(
        &mut self,
        frame: crate::demuxer::Frame<'_>,
        metadata: crate::demuxer::PacketMetadata,
        out: &mut SampleBuffer,
    ) -> DecodeResult<crate::codec::DecodedPacket> {
        let crate::demuxer::Frame {
            data: bytes,
            pts: _pts,
            packet_desc: _packet_desc,
            ..
        } = frame;
        if !bytes.is_empty() {
            self.tail_pending = true;
            out.clear();
            return Ok(crate::codec::DecodedPacket::new(0, metadata));
        }

        self.empty_decode_calls.fetch_add(1, Ordering::SeqCst);
        if !self.tail_pending {
            out.clear();
            return Ok(crate::codec::DecodedPacket::new(0, metadata));
        }
        self.tail_pending = false;
        super::write_silent_test_frame(&self.pcm, self.spec, self.tail_frames, out)
            .map(|frames| crate::codec::DecodedPacket::new(frames, metadata))
    }

    fn flush(&mut self) -> DecodeResult<()> {
        self.tail_pending = false;
        Ok(())
    }

    fn needs_eof_drain(&self, _source_sample_rate: u32) -> bool {
        true
    }

    fn spec(&self) -> AudioSpec {
        self.spec
    }
}
