use std::ops::Range;

use kithara_bufpool::{HasPool, PoolError, PoolRegion, SampleBuffer};
use kithara_signal::FrameCount;

use super::track::PlayerTrack;
use crate::bridge::RtMetrics;

/// What a slot still sounds of the consumer a `Replace` took out of it: its next frames, ramped
/// down to silence, played under the consumer that took its place.
pub(crate) struct SlotTail {
    left: SampleBuffer,
    right: SampleBuffer,
    incoming_left: SampleBuffer,
    incoming_right: SampleBuffer,
    /// Frames the tail holds.
    len: usize,
    /// Frames of it already mixed.
    pos: usize,
}

impl SlotTail {
    /// A silent tail of `frames` frames, allocated once from `pools`.
    pub(crate) fn new<S>(pools: &PoolRegion<S>, frames: FrameCount) -> Result<Self, PoolError>
    where
        S: HasPool<f32>,
    {
        Ok(Self {
            left: pools.get_with_len::<f32>(frames.get())?,
            right: pools.get_with_len::<f32>(frames.get())?,
            incoming_left: pools.get_with_len::<f32>(frames.get())?,
            incoming_right: pools.get_with_len::<f32>(frames.get())?,
            len: 0,
            pos: 0,
        })
    }

    /// Fill the tail from `track`'s next frames, ramped from its gain down to silence.
    pub(crate) fn fill(
        &mut self,
        track: &mut PlayerTrack,
        frames: usize,
        metrics: &RtMetrics,
        budget: &mut usize,
    ) {
        let remaining = self.len - self.pos;
        self.left.copy_within(self.pos..self.len, 0);
        self.right.copy_within(self.pos..self.len, 0);
        let frames = frames.min(self.left.len()).min(self.right.len());
        let written = track.read_tail(
            &mut self.incoming_left[..frames],
            &mut self.incoming_right[..frames],
            metrics,
            budget,
        );
        self.len = remaining.max(written);
        self.left[remaining..self.len].fill(0.0);
        self.right[remaining..self.len].fill(0.0);
        kithara_dsp::add(&self.incoming_left[..written], &mut self.left[..written]);
        kithara_dsp::add(&self.incoming_right[..written], &mut self.right[..written]);
        self.pos = 0;
    }

    pub(crate) fn advance(&mut self, frames: usize) {
        self.pos = self.pos.saturating_add(frames).min(self.len);
    }

    /// Add the tail's next frames into `range` of a stereo pair.
    pub(crate) fn mix(&mut self, left: &mut [f32], right: &mut [f32], range: Range<usize>) {
        let frames = range.len().min(self.len - self.pos);
        if frames == 0 {
            return;
        }
        let from = self.pos..self.pos + frames;
        let to = range.start..range.start + frames;
        kithara_dsp::add(&self.left[from.clone()], &mut left[to.clone()]);
        kithara_dsp::add(&self.right[from], &mut right[to]);
        self.pos += frames;
    }

    /// Whether frames are left to mix.
    pub(crate) const fn is_sounding(&self) -> bool {
        self.pos < self.len
    }

    pub(crate) fn capacity(&self) -> usize {
        self.left.len().min(self.right.len())
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use kithara_platform::{sync::Arc, time::Duration};
    use kithara_signal::{AudioSpec, SegmentId};
    use kithara_test_utils::kithara;
    use num_traits::ToPrimitive;

    use super::*;
    use crate::{
        CrossfadeSettings,
        bridge::Fade,
        rt::track::{PcmConsumer, PlayerResource},
        worker::{
            PcmPacket,
            packet_tests::{PacketRing, chunk},
        },
    };

    #[kithara::test]
    fn tail_refill_adds_only_written_frames_after_compaction() {
        let pools = crate::test_pools::pools();
        let spec = AudioSpec::new(2, NonZeroU32::new(48_000).expect("rate"));
        for available in [0, 1, 3, 7, 257, 513] {
            for requested in [0, 1, 5, 7, 256, 513] {
                let mut ring = PacketRing::new(spec, Duration::from_secs(1), 2);
                if available > 0 {
                    ring.push(PcmPacket::Chunk(Box::new(chunk(
                        spec,
                        SegmentId::FIRST,
                        0,
                        0,
                        &vec![0.5; available * 2],
                    ))));
                }
                let resource = PlayerResource::new(
                    PcmConsumer::new(ring.receiver.take().expect("receiver")),
                    Arc::from("tail"),
                    &pools,
                )
                .expect("resource");
                let mut track = PlayerTrack::builder()
                    .item_id(kithara_events::TrackId::allocate())
                    .sample_rate(spec.sample_rate)
                    .build(Box::new(resource));
                track.start(Fade::Crossfade(CrossfadeSettings {
                    duration: 0.0,
                    ..Default::default()
                }));
                let mut tail = SlotTail::new(&pools, FrameCount::new(514)).expect("tail");
                for (left, value) in tail.left.iter_mut().zip(
                    [0.125, 0.25, 0.375, 0.5, 0.625, 0.75, 0.875, 1.0]
                        .into_iter()
                        .cycle(),
                ) {
                    *left = value;
                }
                for (right, left) in tail.right.iter_mut().zip(tail.left.iter()) {
                    *right = -*left;
                }
                tail.len = 5;
                tail.pos = 2;
                let written = available.min(requested);
                let mut expected_left = tail.left.to_vec();
                let mut expected_right = tail.right.to_vec();
                expected_left.copy_within(2..5, 0);
                expected_right.copy_within(2..5, 0);
                let len = 3.max(written);
                expected_left[3..len].fill(0.0);
                expected_right[3..len].fill(0.0);
                for frame in 0..written {
                    let progress = if requested <= 1 {
                        1.0
                    } else {
                        frame.to_f32().expect("frame") / (requested - 1).to_f32().expect("length")
                    };
                    let incoming = 0.5 * (1.0 - progress);
                    expected_left[frame] += incoming;
                    expected_right[frame] += incoming;
                }
                tail.fill(&mut track, requested, &RtMetrics::default(), &mut 16);
                assert_eq!(&*tail.left, expected_left);
                assert_eq!(&*tail.right, expected_right);
                assert_eq!(tail.len, len);
                assert_eq!(tail.pos, 0);
            }
        }
    }

    #[kithara::test]
    fn tail_mix_preserves_offsets_pcm_and_remaining_position() {
        for len in [0, 1, 7, 513] {
            let mut tail =
                SlotTail::new(&crate::test_pools::pools(), FrameCount::new(514)).expect("tail");
            tail.len = len;
            for (index, (left, right)) in
                tail.left.iter_mut().zip(tail.right.iter_mut()).enumerate()
            {
                *left = if index % 2 == 0 { -0.0 } else { 0.75 };
                *right = -0.5;
            }
            for frames in [0, 3, 512, 1] {
                let mut left = vec![-0.0; 520];
                let mut right = vec![-0.0; 519];
                let (mut expected_left, mut expected_right) = (left.clone(), right.clone());
                let count = frames.min(len - tail.pos);
                let from = tail.pos..tail.pos + count;
                let to = 2..2 + count;
                for (out, sample) in expected_left[to.clone()]
                    .iter_mut()
                    .zip(&tail.left[from.clone()])
                {
                    *out += sample;
                }
                for (out, sample) in expected_right[to].iter_mut().zip(&tail.right[from]) {
                    *out += sample;
                }
                let pos = tail.pos;
                tail.mix(&mut left, &mut right, 2..2 + frames);
                assert_eq!(
                    left.iter().map(|x| x.to_bits()).collect::<Vec<_>>(),
                    expected_left
                        .iter()
                        .map(|x| x.to_bits())
                        .collect::<Vec<_>>()
                );
                assert_eq!(
                    right.iter().map(|x| x.to_bits()).collect::<Vec<_>>(),
                    expected_right
                        .iter()
                        .map(|x| x.to_bits())
                        .collect::<Vec<_>>()
                );
                assert_eq!(tail.pos, pos + count);
                assert_eq!(tail.is_sounding(), tail.pos < len);
            }
        }
    }
}
