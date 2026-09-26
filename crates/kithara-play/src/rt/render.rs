use std::num::NonZeroU32;

use firewheel::node::ProcBuffers;
use kithara_bufpool::{HasPool, PoolRegion, SampleBuffer};
use kithara_dsp::{
    fade::FadeCurve,
    param::{Mix, MixDSP, SmootherConfig},
};
use kithara_warp::RenderContext;
use num_traits::cast::AsPrimitive;
use ringbuf::HeapProd;
use smallvec::SmallVec;
use tracing::warn;

use super::{
    processor::{PlayerNodeProcessor, StreamShape},
    track::{RtSink, TrackReadOutcome},
};
use crate::{
    bridge::{PlayerNotification, RtMetrics, TrackState},
    rt::{TrackSlot, TrackSlots},
};

type ActiveTrackEntry = (usize, TrackSlot, bool);

#[derive(Clone, Copy)]
struct Handover {
    offset: usize,
}

pub(crate) struct RenderTargets<'a> {
    pub(crate) notification_tx: &'a mut HeapProd<PlayerNotification>,
    pub(crate) metrics: &'a RtMetrics,
    pub(crate) tracks: &'a mut TrackSlots<{ PlayerNodeProcessor::MAX_TRACKS }>,
    /// Slot seek epoch published when this block started rendering.
    pub(crate) seek_epoch: u64,
}

pub(crate) struct RenderPass {
    gate: MixDSP,
    scratch_bufs: [SampleBuffer; Self::SCRATCH_BUF_COUNT],
    priming: bool,
    capacity: usize,
}

impl RenderPass {
    const GATE_CURVE: FadeCurve = FadeCurve::Linear;
    const MIN_STEREO: usize = 2;
    const SCRATCH_BUF_COUNT: usize = 4;

    pub(crate) fn new<S>(
        pools: &PoolRegion<S>,
        shape: StreamShape,
        gate_smoothing: SmootherConfig,
    ) -> Self
    where
        S: HasPool<f32>,
    {
        let mut pass = Self {
            scratch_bufs: std::array::from_fn(|_| pools.get::<f32>()),
            capacity: 0,
            priming: true,
            gate: MixDSP::new(
                Mix::FULLY_WET,
                Self::GATE_CURVE,
                gate_smoothing,
                shape.sample_rate,
            ),
        };
        pass.resize(shape.max_block_frames.get().as_());
        pass
    }

    /// Render audio for all active tracks into the output buffers.
    ///
    /// Frames are clamped rather than grown, since growing a pooled buffer here would allocate on
    /// the audio thread; frames past the clamp are already silence-filled.
    pub(crate) fn render_audio(
        &mut self,
        context: Option<&RenderContext>,
        targets: RenderTargets<'_>,
        buffers: &mut ProcBuffers,
        frames: usize,
        is_playing: bool,
    ) -> (bool, Option<(f64, f64)>) {
        let mut playback_started = false;
        let mut leading_outcome_pos_dur: Option<(f64, f64)> = None;

        if buffers.outputs.len() < Self::MIN_STEREO {
            return (false, None);
        }

        for ch_buffer in buffers.outputs.iter_mut() {
            ch_buffer[..frames].fill(0.0);
        }

        let frames = frames.min(self.capacity);

        self.gate.set_mix(
            if is_playing {
                Mix::FULLY_DRY
            } else {
                Mix::FULLY_WET
            },
            Self::GATE_CURVE,
        );
        if self.priming {
            self.priming = false;
            self.gate.reset_to_target();
        }
        if !is_playing && self.gate.has_settled() {
            return (false, None);
        }

        let (read, bus) = self.scratch_bufs.split_at_mut(Self::MIN_STEREO);
        let (read_buf0, read_buf1) = read.split_at_mut(1);
        let (bus_buf0, bus_buf1) = bus.split_at_mut(1);
        let mut read_bufs = [&mut read_buf0[0][..frames], &mut read_buf1[0][..frames]];
        let mut bus_bufs = [&mut bus_buf0[0][..frames], &mut bus_buf1[0][..frames]];
        for ch_buffer in &mut bus_bufs {
            ch_buffer.fill(0.0);
        }
        let tracks = targets.tracks;
        let mut sink = RtSink::new(targets.notification_tx, targets.metrics, targets.seek_epoch);
        let loaded_tracks: SmallVec<[(TrackSlot, TrackState); PlayerNodeProcessor::MAX_TRACKS]> =
            tracks
                .iter()
                .map(|(idx, track)| (idx, track.state()))
                .collect();
        let active_tracks: SmallVec<[ActiveTrackEntry; PlayerNodeProcessor::MAX_TRACKS]> =
            loaded_tracks
                .iter()
                .enumerate()
                .filter(|(_, (_, state))| state.is_playing())
                .map(|(loaded_idx, (idx, state))| (loaded_idx, *idx, state.is_leading()))
                .collect();
        let mut active_slots = [false; PlayerNodeProcessor::MAX_TRACKS];
        for (loaded_idx, _, _) in &active_tracks {
            active_slots[*loaded_idx] = true;
        }
        let mut skip_tracks = [false; PlayerNodeProcessor::MAX_TRACKS];

        for (track_idx, (_arena_slot, track_handle, was_leading)) in
            active_tracks.iter().enumerate()
        {
            if skip_tracks[track_idx] {
                continue;
            }

            let mut read_outcome = {
                let Some(outcome) = tracks.at_mut(*track_handle).map(|track| {
                    track.render(context, &mut read_bufs, &mut bus_bufs, 0..frames, &mut sink)
                }) else {
                    continue;
                };
                playback_started = true;
                outcome
            };

            if *was_leading {
                if let Some(snapshot) = outcome_position_duration(&read_outcome) {
                    leading_outcome_pos_dur = Some(snapshot);
                }

                let mut handover = initial_handover(&read_outcome);

                for (next_idx, (_, next_handle, next_is_leading)) in
                    active_tracks.iter().enumerate()
                {
                    let Some(handoff) = handover else {
                        break;
                    };
                    let offset = handoff.offset;
                    if next_idx == track_idx || skip_tracks[next_idx] || !*next_is_leading {
                        continue;
                    }
                    if offset >= frames {
                        break;
                    }

                    let Some(outcome) = tracks.at_mut(*next_handle).map(|track| {
                        track.render(
                            context,
                            &mut read_bufs,
                            &mut bus_bufs,
                            offset..frames,
                            &mut sink,
                        )
                    }) else {
                        continue;
                    };
                    read_outcome = outcome;
                    skip_tracks[next_idx] = true;

                    if let Some(snapshot) = outcome_position_duration(&read_outcome) {
                        leading_outcome_pos_dur = Some(snapshot);
                    }

                    handover = next_handover(&read_outcome, offset);
                }

                if let Some(handoff) = handover
                    && handoff.offset < frames
                {
                    let offset = handoff.offset;
                    for (next_arena_idx, (next_handle, next_state)) in
                        loaded_tracks.iter().enumerate()
                    {
                        if *next_state != TrackState::Preloading || active_slots[next_arena_idx] {
                            continue;
                        }

                        let Some(next_track) = tracks.at_mut(*next_handle) else {
                            continue;
                        };
                        next_track.play();
                        next_track.render(
                            context,
                            &mut read_bufs,
                            &mut bus_bufs,
                            offset..frames,
                            &mut sink,
                        );
                        break;
                    }
                }
            }
        }

        let (out_left, out_right) = buffers.outputs.split_at_mut(1);
        self.gate.mix_dry_into_wet_stereo(
            bus_bufs[0],
            bus_bufs[1],
            &mut out_left[0][..frames],
            &mut out_right[0][..frames],
            frames,
        );

        (playback_started, leading_outcome_pos_dur)
    }

    pub(crate) fn resize(&mut self, max_frames: usize) {
        let mut capacity = usize::MAX;
        for buf in &mut self.scratch_bufs {
            if buf.ensure_len(max_frames).is_err() {
                warn!(
                    max_frames,
                    held = buf.len(),
                    "sample pool budget cannot afford the render scratch; blocks are clamped"
                );
            }
            buf.fill(0.0);
            capacity = capacity.min(buf.len());
        }
        self.capacity = capacity;
    }

    pub(crate) fn update_sample_rate(&mut self, sample_rate: NonZeroU32) {
        self.gate.update_sample_rate(sample_rate);
    }
}

const fn initial_handover(read_outcome: &TrackReadOutcome) -> Option<Handover> {
    match read_outcome {
        TrackReadOutcome::Partial { frames, .. } => Some(Handover { offset: *frames }),
        TrackReadOutcome::Eof | TrackReadOutcome::Failed(_) => Some(Handover { offset: 0 }),
        TrackReadOutcome::Full { .. } => None,
    }
}

const fn next_handover(read_outcome: &TrackReadOutcome, offset: usize) -> Option<Handover> {
    match read_outcome {
        TrackReadOutcome::Full { .. } => None,
        TrackReadOutcome::Partial { frames, .. } => Some(Handover {
            offset: offset.saturating_add(*frames),
        }),
        TrackReadOutcome::Eof | TrackReadOutcome::Failed(_) => Some(Handover { offset }),
    }
}

const fn outcome_position_duration(outcome: &TrackReadOutcome) -> Option<(f64, f64)> {
    match *outcome {
        TrackReadOutcome::Full {
            position, duration, ..
        } => Some((position, duration)),
        TrackReadOutcome::Partial { duration, .. } => Some((duration, duration)),
        TrackReadOutcome::Eof | TrackReadOutcome::Failed(_) => None,
    }
}

pub(super) const fn eviction_priority(state: TrackState) -> u8 {
    const EVICT_PRELOADING: u8 = 2;
    const EVICT_FADING_IN: u8 = 3;
    const EVICT_PLAYING: u8 = 4;

    match state {
        TrackState::Finished => 0,
        TrackState::FadingOut => 1,
        TrackState::Preloading => EVICT_PRELOADING,
        TrackState::FadingIn => EVICT_FADING_IN,
        TrackState::Playing => EVICT_PLAYING,
    }
}
