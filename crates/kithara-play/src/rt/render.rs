use std::num::NonZeroU32;

use firewheel::{
    dsp::{
        fade::FadeCurve,
        mix::{Mix, MixDSP},
    },
    node::ProcBuffers,
    param::smoother::{SmoothedParam, SmootherConfig},
};
use kithara_bufpool::{HasPool, PoolRegion, SampleBuffer};
use kithara_platform::sync::Arc;
use kithara_test_utils::kithara;
use kithara_warp::{RenderContext, StretchControls};
use num_traits::cast::AsPrimitive;
use ringbuf::HeapProd;
use smallvec::SmallVec;
use tracing::warn;
use triple_buffer::Output;

use super::{
    processor::{PlayerNodeProcessor, StreamShape},
    track::{PreparedLaunchReadiness, RtSink, TrackReadOutcome},
};
use crate::{
    bridge::{PlayerNotification, RtMetrics, TrackState},
    rt::{TrackSlot, TrackSlots},
    sync::DeckGrid,
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
    grid: Output<DeckGrid>,
    stretch: Arc<StretchControls>,
    rate: SmoothedParam,
    gate: MixDSP,
    scratch_bufs: [SampleBuffer; Self::SCRATCH_BUF_COUNT],
    priming: bool,
    capacity: usize,
}

impl RenderPass {
    const GATE_CURVE: FadeCurve = FadeCurve::Linear;

    const MIN_STEREO: usize = 2;

    const SCRATCH_BUF_COUNT: usize = 6;

    pub(crate) fn new<S>(
        pools: &PoolRegion<S>,
        shape: StreamShape,
        stretch: Arc<StretchControls>,
        smoothing: SmootherConfig,
        grid: Output<DeckGrid>,
        gate_smoothing: SmootherConfig,
    ) -> Self
    where
        S: HasPool<f32>,
    {
        let mut pass = Self {
            grid,
            rate: SmoothedParam::new(stretch.speed(), smoothing, shape.sample_rate),
            stretch,
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
    pub(crate) fn render_audio(
        &mut self,
        context: Option<&RenderContext>,
        targets: RenderTargets<'_>,
        buffers: &mut ProcBuffers,
        frames: usize,
        is_playing: bool,
    ) -> (bool, bool, Option<(f64, f64)>) {
        if buffers.outputs.len() < Self::MIN_STEREO {
            return (false, false, None);
        }

        for ch_buffer in buffers.outputs.iter_mut() {
            ch_buffer[..frames].fill(0.0);
        }

        // WHY: Growing a pooled buffer here would allocate on the audio thread. The fill above already covered the frames past the clamp
        // with silence.
        let frames = frames.min(self.capacity);
        let context = self.render_context(context, frames);
        self.render_tracks(context.as_ref(), targets, buffers, frames, is_playing)
    }

    fn render_tracks(
        &mut self,
        context: Option<&RenderContext>,
        targets: RenderTargets<'_>,
        buffers: &mut ProcBuffers,
        frames: usize,
        is_playing: bool,
    ) -> (bool, bool, Option<(f64, f64)>) {
        let mut outputs_modified = false;
        // Only a prepared launch is allowed to promote the shared playback
        // state. Ordinary tracks may still render while the pause gate drains
        // its fade-out, but that must not turn a pause back into playback.
        let mut prepared_launch_started = false;
        let mut leading_outcome_pos_dur: Option<(f64, f64)> = None;
        let tracks = targets.tracks;
        let prepared_ready = prepared_launch_ready(tracks, context, frames, is_playing);
        let is_playing = is_playing || prepared_ready.is_some();
        self.update_gate(is_playing);
        // WHY: A closed gate outputs silence whatever the tracks hold, so readers stop only once its ramp has run out.
        if !is_playing && self.gate.has_settled() {
            return (false, false, None);
        }

        let (read, rest) = self.scratch_bufs.split_at_mut(Self::MIN_STEREO);
        let (mix, bus) = rest.split_at_mut(Self::MIN_STEREO);
        let (read_buf0, read_buf1) = read.split_at_mut(1);
        let (mix_buf0, mix_buf1) = mix.split_at_mut(1);
        let (bus_buf0, bus_buf1) = bus.split_at_mut(1);
        let mut read_bufs = [&mut read_buf0[0][..frames], &mut read_buf1[0][..frames]];
        let mut mix_bufs = [&mut mix_buf0[0][..frames], &mut mix_buf1[0][..frames]];
        let mut bus_bufs = [&mut bus_buf0[0][..frames], &mut bus_buf1[0][..frames]];
        for ch_buffer in &mut bus_bufs {
            ch_buffer.fill(0.0);
        }
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
                .filter(|(_, (slot, state))| {
                    state.is_playing()
                        || prepared_ready.is_some_and(|(prepared, _)| prepared == *slot)
                })
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
            if prepared_ready.is_some_and(|(slot, _)| slot != *track_handle) {
                continue;
            }
            if skip_tracks[track_idx] {
                continue;
            }

            for ch_buffer in &mut mix_bufs {
                ch_buffer.fill(0.0);
            }

            let mut read_outcome = {
                let range = prepared_ready
                    .filter(|(slot, _)| *slot == *track_handle)
                    .map_or(0..frames, |(_, prefix)| prefix..frames);
                let Some(outcome) = tracks.at_mut(*track_handle).map(|track| {
                    track.render(context, &mut read_bufs, &mut mix_bufs, range, &mut sink)
                }) else {
                    continue;
                };
                let prepared_track = prepared_ready.is_some_and(|(slot, _)| slot == *track_handle);
                let rendered_frames = matches!(outcome, TrackReadOutcome::Full { frames, .. } | TrackReadOutcome::Partial { frames, .. } if frames > 0);
                outputs_modified |= !prepared_track || rendered_frames;
                prepared_launch_started |= prepared_track && rendered_frames;
                outcome
            };

            if *was_leading && prepared_ready.is_none() {
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
                            &mut mix_bufs,
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
                            &mut mix_bufs,
                            offset..frames,
                            &mut sink,
                        );
                        break;
                    }
                }
            }

            for (bus_ch, mix_ch) in bus_bufs.iter_mut().zip(mix_bufs.iter()) {
                bus_ch
                    .iter_mut()
                    .zip(mix_ch.iter())
                    .for_each(|(bus_sample, &mix_sample)| *bus_sample += mix_sample);
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

        (
            outputs_modified,
            prepared_launch_started,
            leading_outcome_pos_dur,
        )
    }

    fn update_gate(&mut self, is_playing: bool) {
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
    }

    fn render_context(
        &mut self,
        context: Option<&RenderContext>,
        frames: usize,
    ) -> Option<RenderContext> {
        let target = self.stretch.rate_target();
        self.rate.set_value(target.speed());
        let mut multiplier = self.rate.target_value();
        for _ in 0..frames {
            multiplier = self.rate.next_smoothed();
        }
        self.rate.settle();
        kithara::probe_event!(
            rate_smoothed,
            frames = frames,
            target_bits = target.speed().to_bits(),
            multiplier_bits = multiplier.to_bits()
        );
        let grid = *self.grid.read();
        context.and_then(|context| grid.project(context, target.with_speed(multiplier)))
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
        self.rate.update_sample_rate(sample_rate);
    }
}

fn prepared_launch_ready(
    tracks: &mut TrackSlots<{ PlayerNodeProcessor::MAX_TRACKS }>,
    context: Option<&RenderContext>,
    frames: usize,
    is_playing: bool,
) -> Option<(TrackSlot, usize)> {
    (!is_playing)
        .then(|| {
            context.and_then(|context| {
                tracks.iter_mut().find_map(|(slot, track)| {
                    match track.prepared_launch_readiness(context, frames) {
                        PreparedLaunchReadiness::Ready { prefix_frames } => {
                            Some((slot, prefix_frames))
                        }
                        PreparedLaunchReadiness::NotReady => None,
                    }
                })
            })
        })
        .flatten()
}

const fn initial_handover(read_outcome: &TrackReadOutcome) -> Option<Handover> {
    match read_outcome {
        TrackReadOutcome::Partial { frames, .. } => Some(Handover { offset: *frames }),
        TrackReadOutcome::Eof | TrackReadOutcome::Failed => Some(Handover { offset: 0 }),
        TrackReadOutcome::Full { .. } => None,
    }
}

const fn next_handover(read_outcome: &TrackReadOutcome, offset: usize) -> Option<Handover> {
    match read_outcome {
        TrackReadOutcome::Full { .. } => None,
        TrackReadOutcome::Partial { frames, .. } => Some(Handover {
            offset: offset.saturating_add(*frames),
        }),
        TrackReadOutcome::Eof | TrackReadOutcome::Failed => Some(Handover { offset }),
    }
}

const fn outcome_position_duration(outcome: &TrackReadOutcome) -> Option<(f64, f64)> {
    match *outcome {
        TrackReadOutcome::Full {
            position, duration, ..
        } => Some((position, duration)),
        TrackReadOutcome::Partial { duration, .. } => Some((duration, duration)),
        TrackReadOutcome::Eof | TrackReadOutcome::Failed => None,
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
