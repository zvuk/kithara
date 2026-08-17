use std::num::NonZeroU32;

use firewheel::{
    dsp::{
        fade::FadeCurve,
        filter::smoothing_filter::DEFAULT_SETTLE_EPSILON,
        mix::{Mix, MixDSP},
    },
    node::ProcBuffers,
    param::smoother::SmootherConfig,
};
use kithara_audio::{PresentationCursor, SessionFrame};
use kithara_bufpool::{PcmBuf, PcmPool};
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
    pub(crate) tracks: &'a mut TrackSlots<{ PlayerNodeProcessor::MAX_TRACKS }>,
    pub(crate) notification_tx: &'a mut HeapProd<PlayerNotification>,
    pub(crate) metrics: &'a RtMetrics,
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct RenderOutcome {
    pub(crate) playback_started: bool,
    pub(crate) leading_position_duration: Option<(f64, f64)>,
    pub(crate) presentation: Option<PresentationCursor>,
}

pub(crate) struct RenderPass {
    scratch_bufs: [PcmBuf; Self::SCRATCH_BUF_COUNT],
    capacity: usize,
    gate: MixDSP,
    priming: bool,
}

impl RenderPass {
    const GATE_CURVE: FadeCurve = FadeCurve::Linear;

    const GATE_SMOOTH_SECONDS: f32 = 0.005;

    const MIN_STEREO: usize = 2;

    const SCRATCH_BUF_COUNT: usize = 6;

    pub(crate) fn new(pool: &PcmPool, shape: StreamShape) -> Self {
        let mut pass = Self {
            scratch_bufs: std::array::from_fn(|_| pool.get()),
            capacity: 0,
            priming: true,
            gate: MixDSP::new(
                Mix::FULLY_WET,
                Self::GATE_CURVE,
                SmootherConfig {
                    smooth_seconds: Self::GATE_SMOOTH_SECONDS,
                    settle_epsilon: DEFAULT_SETTLE_EPSILON,
                },
                shape.sample_rate,
            ),
        };
        pass.resize(shape.max_block_frames.get().as_());
        pass
    }

    /// Render audio for all active tracks into the output buffers.
    pub(crate) fn render_audio(
        &mut self,
        targets: RenderTargets<'_>,
        buffers: &mut ProcBuffers,
        frames: usize,
        is_playing: bool,
        block_frame: Option<SessionFrame>,
    ) -> RenderOutcome {
        let mut playback_started = false;
        let mut leading_outcome_pos_dur: Option<(f64, f64)> = None;
        let mut leading_presentation = None;
        let tracks = targets.tracks;
        let Some((frames, block_frame)) =
            self.prepare_block(tracks, buffers, frames, is_playing, block_frame)
        else {
            return RenderOutcome::default();
        };

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
        let mut sink = RtSink::new(targets.notification_tx, targets.metrics);
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

            for ch_buffer in &mut mix_bufs {
                ch_buffer.fill(0.0);
            }

            let mut read_outcome = {
                let Some(outcome) = tracks.at_mut(*track_handle).map(|track| {
                    track.read(
                        &mut read_bufs,
                        &mut mix_bufs,
                        0..frames,
                        block_frame,
                        &mut sink,
                    )
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
                leading_presentation = outcome_presentation(&read_outcome);

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
                        track.read(
                            &mut read_bufs,
                            &mut mix_bufs,
                            offset..frames,
                            block_frame,
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
                    leading_presentation = outcome_presentation(&read_outcome);

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
                        let outcome = next_track.read(
                            &mut read_bufs,
                            &mut mix_bufs,
                            offset..frames,
                            block_frame,
                            &mut sink,
                        );
                        leading_outcome_pos_dur = outcome_position_duration(&outcome);
                        leading_presentation = outcome_presentation(&outcome);
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

        RenderOutcome {
            playback_started,
            leading_position_duration: leading_outcome_pos_dur,
            presentation: leading_presentation,
        }
    }

    fn prepare_block(
        &mut self,
        tracks: &mut TrackSlots<{ PlayerNodeProcessor::MAX_TRACKS }>,
        buffers: &mut ProcBuffers,
        frames: usize,
        is_playing: bool,
        block_frame: Option<SessionFrame>,
    ) -> Option<(usize, Option<SessionFrame>)> {
        if buffers.outputs.len() < Self::MIN_STEREO {
            tracks
                .iter_mut()
                .for_each(|(_, track)| track.invalidate_presentation());
            return None;
        }
        for ch_buffer in buffers.outputs.iter_mut() {
            ch_buffer[..frames].fill(0.0);
        }
        let clamped = frames > self.capacity;
        if clamped {
            tracks
                .iter_mut()
                .for_each(|(_, track)| track.invalidate_presentation());
        }
        let frames = frames.min(self.capacity);
        let block_frame = block_frame.filter(|_| !clamped);
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
            tracks
                .iter_mut()
                .for_each(|(_, track)| track.invalidate_presentation());
            return None;
        }
        Some((frames, block_frame))
    }

    pub(crate) fn update_sample_rate(&mut self, sample_rate: NonZeroU32) {
        self.gate.update_sample_rate(sample_rate);
    }

    pub(crate) fn resize(&mut self, max_frames: usize) {
        let mut capacity = usize::MAX;
        for buf in &mut self.scratch_bufs {
            if buf.ensure_len(max_frames).is_err() {
                warn!(
                    max_frames,
                    held = buf.len(),
                    "PCM pool budget cannot afford the render scratch; blocks are clamped"
                );
            }
            buf.fill(0.0);
            capacity = capacity.min(buf.len());
        }
        self.capacity = capacity;
    }
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

const fn outcome_presentation(outcome: &TrackReadOutcome) -> Option<PresentationCursor> {
    match *outcome {
        TrackReadOutcome::Full { presentation, .. }
        | TrackReadOutcome::Partial { presentation, .. } => presentation,
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
