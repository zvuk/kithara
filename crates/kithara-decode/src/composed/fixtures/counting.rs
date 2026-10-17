use std::{
    collections::VecDeque,
    sync::atomic::{AtomicU32, Ordering},
};

use kithara_bufpool::SampleBuffer;
use kithara_platform::sync::Arc;
use kithara_signal::AudioSpec;

use crate::{codec::FrameCodec, error::DecodeResult};

pub(in crate::composed) struct CountingCodec {
    pub(in crate::composed) decode_calls: Arc<AtomicU32>,
    pub(in crate::composed) flush_calls: Arc<AtomicU32>,
    pub(in crate::composed) spec: AudioSpec,
    pub(in crate::composed) frames_per_call: u32,
    pcm: Vec<f32>,
    frames: VecDeque<u32>,
}

impl CountingCodec {
    pub(in crate::composed) fn new(pcm: Vec<f32>, spec: AudioSpec, frames_per_call: u32) -> Self {
        Self {
            pcm,
            spec,
            frames_per_call,
            frames: VecDeque::new(),
            decode_calls: Arc::new(AtomicU32::new(0)),
            flush_calls: Arc::new(AtomicU32::new(0)),
        }
    }

    pub(in crate::composed) fn with_frames(
        mut self,
        frames: impl IntoIterator<Item = u32>,
    ) -> Self {
        self.frames.extend(frames);
        self
    }
}

impl FrameCodec for CountingCodec {
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
        self.decode_calls.fetch_add(1, Ordering::SeqCst);
        let frames = self.frames.pop_front().unwrap_or(self.frames_per_call);
        super::write_silent_test_frame(&self.pcm, self.spec, frames, out)
            .map(|frames| crate::codec::DecodedPacket::new(frames, metadata))
    }

    fn flush(&mut self) -> DecodeResult<()> {
        self.flush_calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn spec(&self) -> AudioSpec {
        self.spec
    }
}
