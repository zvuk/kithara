use kithara_bufpool::SampleBuffer;
use kithara_platform::time::Duration;
use kithara_signal::AudioSpec;

use crate::{codec::FrameCodec, error::DecodeResult};

pub(in crate::composed) struct ConstFrameCodec {
    spec: AudioSpec,
    pcm: Vec<f32>,
    frames_per_call: u32,
}

pub(in crate::composed) struct LaggedQueueCodec {
    spec: AudioSpec,
    decoded_pts: Duration,
    pending_pts: Option<Duration>,
    pcm: Vec<f32>,
    frames_per_call: u32,
}

impl ConstFrameCodec {
    pub(in crate::composed) fn new(pcm: Vec<f32>, spec: AudioSpec, frames_per_call: u32) -> Self {
        Self {
            spec,
            pcm,
            frames_per_call,
        }
    }
}

impl LaggedQueueCodec {
    pub(in crate::composed) fn new(pcm: Vec<f32>, spec: AudioSpec, frames_per_call: u32) -> Self {
        Self {
            pcm,
            spec,
            frames_per_call,
            pending_pts: None,
            decoded_pts: Duration::ZERO,
        }
    }
}

impl FrameCodec for ConstFrameCodec {
    fn decode_frame(
        &mut self,
        frame: crate::demuxer::Frame<'_>,
        metadata: crate::demuxer::PacketMetadata,
        out: &mut SampleBuffer,
    ) -> DecodeResult<crate::codec::DecodedPacket> {
        let crate::demuxer::Frame {
            data: _bytes,
            pts: _pts,
            packet_desc: _packet_desc,
            ..
        } = frame;
        super::write_silent_test_frame(&self.pcm, self.spec, self.frames_per_call, out)
            .map(|frames| crate::codec::DecodedPacket::new(frames, metadata))
    }

    fn flush(&mut self) -> DecodeResult<()> {
        Ok(())
    }

    fn spec(&self) -> AudioSpec {
        self.spec
    }
}

impl FrameCodec for LaggedQueueCodec {
    fn needs_eof_drain(&self, _source_rate: u32) -> bool {
        true
    }

    fn decode_frame(
        &mut self,
        frame: crate::demuxer::Frame<'_>,
        metadata: crate::demuxer::PacketMetadata,
        out: &mut SampleBuffer,
    ) -> DecodeResult<crate::codec::DecodedPacket> {
        let crate::demuxer::Frame {
            data: bytes,
            pts,
            packet_desc: _packet_desc,
            ..
        } = frame;
        let pending = if bytes.is_empty() {
            self.pending_pts.take()
        } else {
            self.pending_pts.replace(pts)
        };
        let Some(decoded_pts) = pending else {
            out.clear();
            return Ok(crate::codec::DecodedPacket::new(0, metadata));
        };
        self.decoded_pts = decoded_pts;
        super::write_silent_test_frame(&self.pcm, self.spec, self.frames_per_call, out)
            .map(|frames| crate::codec::DecodedPacket::new(frames, metadata))
    }

    fn decoded_pts(&self) -> Option<Duration> {
        Some(self.decoded_pts)
    }

    fn flush(&mut self) -> DecodeResult<()> {
        self.pending_pts = None;
        Ok(())
    }

    fn spec(&self) -> AudioSpec {
        self.spec
    }
}
