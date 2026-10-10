use std::{fmt, num::NonZeroUsize};

use kithara_abr::AbrHandle;
use kithara_audio::Audio;
use kithara_decode::TrackMetadata;
use kithara_platform::{
    sync::{Arc, ThreadGate, WaitGate},
    time::Duration,
};
use kithara_signal::AudioSpec;
use kithara_stream::WorkerWake;
use kithara_test_utils::kithara;
use ringbuf::{
    HeapCons, HeapProd, HeapRb,
    traits::{Consumer, Observer, Producer, Split},
};
use triple_buffer::{Input, Output, triple_buffer};

use super::{super::scheduler::StreamWake, packet::PcmPacket};

pub(in crate::worker) struct PcmProducer {
    pub(in crate::worker) forward: HeapProd<PcmPacket>,
    pub(in crate::worker) reverse: HeapCons<PcmPacket>,
    pub(in crate::worker) playing: Output<bool>,
    pub(super) ready: FinalWake,
}

pub(super) struct FinalWake(pub(super) Option<Arc<ThreadGate>>);

impl Drop for FinalWake {
    fn drop(&mut self) {
        if let Some(ready) = &self.0 {
            ready.signal();
        }
    }
}

impl PcmProducer {
    pub(in crate::worker) fn signal(&self) {
        if let Some(ready) = &self.ready.0 {
            ready.signal();
        }
    }
}

/// Receiver of rendered PCM; every packet is returned to the owning lane.
pub struct PcmReceiver {
    pub(super) forward: HeapCons<PcmPacket>,
    pub(super) reverse: HeapProd<PcmPacket>,
    pub(super) playing: Input<bool>,
    pub(super) ready: Option<Arc<ThreadGate>>,
    pub(super) wake: StreamWake,
    pub(super) spec: AudioSpec,
    pub(super) duration: Option<Duration>,
    pub(super) position: Duration,
    pub(super) frontier: Duration,
    pub(super) metadata: TrackMetadata,
    pub(super) abr: Option<AbrHandle>,
}

impl PcmReceiver {
    /// The reverse ring fits forward capacity, the reader-held packet, and one
    /// worker push between recycles, so returning an in-flight packet never fails.
    pub(in crate::worker) fn new<T>(
        capacity: NonZeroUsize,
        block_on_underrun: bool,
        wake: StreamWake,
        audio: &Audio<T>,
        position: Duration,
    ) -> (Self, PcmProducer) {
        let (forward_tx, forward_rx) = HeapRb::new(capacity.get()).split();
        let (reverse_tx, reverse_rx) = HeapRb::new(capacity.get() + 2).split();
        let (playing_tx, playing_rx) = triple_buffer(&false);
        let ready = block_on_underrun.then(|| Arc::new(ThreadGate::default()));
        (
            Self {
                forward: forward_rx,
                reverse: reverse_tx,
                playing: playing_tx,
                ready: ready.clone(),
                wake,
                spec: audio.spec(),
                duration: audio.duration(),
                position,
                frontier: position,
                metadata: audio.metadata().clone(),
                abr: audio.abr_handle(),
            },
            PcmProducer {
                forward: forward_tx,
                reverse: reverse_rx,
                playing: playing_rx,
                ready: FinalWake(ready),
            },
        )
    }

    #[must_use]
    pub const fn spec(&self) -> AudioSpec {
        self.spec
    }

    #[must_use]
    pub fn duration(&self) -> Option<Duration> {
        match self.forward.last() {
            Some(PcmPacket::Chunk(chunk)) if chunk.meta.end_of_track => chunk
                .meta
                .source_span
                .and_then(|span| span.position_at(span.output_frames())),
            _ => self.duration,
        }
    }

    #[must_use]
    pub const fn position(&self) -> Duration {
        self.position
    }

    pub(crate) fn frames_until_eof(&self, lane: crate::LaneFrame) -> Option<usize> {
        let PcmPacket::Chunk(chunk) = self.forward.last()? else {
            return None;
        };
        if !chunk.meta.end_of_track || chunk.meta.segment != lane.segment {
            return None;
        }
        let end = chunk
            .meta
            .lane_frame
            .checked_add(u64::try_from(chunk.frames()).ok()?)?;
        usize::try_from(end.checked_sub(lane.frame)?).ok()
    }

    #[must_use]
    pub fn decoded_frontier(&self) -> Duration {
        match self.forward.last() {
            Some(PcmPacket::Chunk(chunk)) => chunk
                .meta
                .source_span
                .and_then(|span| span.position_at(span.output_frames()))
                .unwrap_or(self.frontier),
            _ => self.frontier,
        }
    }

    #[must_use]
    pub fn cached_span(&self) -> Duration {
        self.decoded_frontier().saturating_sub(self.position)
    }

    #[must_use]
    pub const fn metadata(&self) -> &TrackMetadata {
        &self.metadata
    }

    /// Adaptive bitrate control retained from the one source open.
    #[must_use]
    pub fn abr_handle(&self) -> Option<AbrHandle> {
        self.abr.clone()
    }

    /// Publish transport activity through the ring's deferred wake path.
    pub fn set_playing(&mut self, playing: bool) {
        self.playing.write(playing);
        self.notify();
    }

    /// Record the exact consumed source position supplied by the PCM owner.
    pub fn set_position(&mut self, position: Duration) {
        self.position = position;
    }

    #[must_use]
    pub fn peek(&self) -> Option<&PcmPacket> {
        let packet = self.forward.try_peek();
        if packet.is_none() {
            self.notify();
        }
        packet
    }

    /// Wait for a packet or producer closure only when offline blocking is enabled.
    #[kithara::flash(true)]
    #[kithara::measure(label = "render.pcm.wait")]
    #[kithara::hang_watchdog]
    pub(crate) fn wait_for_packet(&self) {
        let Some(ready) = &self.ready else {
            return;
        };
        loop {
            let since = ready.current();
            if self.forward.try_peek().is_some() || !self.forward.write_is_held() {
                hang_reset!();
                return;
            }
            self.wake.wake();
            hang_park!(|remaining| {
                ready.wait_timeout(since, remaining);
            });
        }
    }

    /// Try to take one packet without waiting, regardless of blocking policy.
    pub fn pop(&mut self) -> Option<PcmPacket> {
        let packet = self.forward.try_pop();
        if let Some(PcmPacket::Chunk(chunk)) = &packet {
            self.spec = chunk.spec();
            if let Some(position) = chunk
                .meta
                .source_span
                .and_then(|span| span.position_at(span.output_frames()))
            {
                self.frontier = position;
                if chunk.meta.end_of_track {
                    self.duration = Some(position);
                }
            }
        }
        self.notify();
        packet
    }

    #[must_use]
    pub fn is_closed(&self) -> bool {
        !self.forward.write_is_held() && self.forward.try_peek().is_none()
    }

    /// Return a packet for off-RT reclamation, retaining it on a full ring.
    ///
    /// # Errors
    /// Returns the original packet when the reverse ring is full.
    pub fn recycle(&mut self, packet: PcmPacket) -> Result<(), PcmPacket> {
        let result = self.reverse.try_push(packet);
        self.notify();
        result
    }

    fn notify(&self) {
        if self.ready.is_some() {
            self.wake.wake();
        } else {
            self.wake.defer();
        }
    }
}

impl fmt::Debug for PcmReceiver {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PcmReceiver")
            .field("spec", &self.spec)
            .field("duration", &self.duration)
            .field("position", &self.position)
            .finish_non_exhaustive()
    }
}
