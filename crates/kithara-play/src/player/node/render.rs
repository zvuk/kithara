use firewheel::node::ProcBuffers;
use kithara_bufpool::{PcmBuf, PcmPool};
use kithara_platform::sync::Arc;
use ringbuf::HeapProd;
use smallvec::SmallVec;
use thunderdome::Index;

use super::{ArenaRegistry, processor::PlayerNodeProcessor};
use crate::{
    bridge::{PlayerNotification, TrackState},
    player::track::{PlayerTrack, TrackReadOutcome, TrackRenderMode},
};

type ActiveTrackEntry = (usize, Index, bool, usize);

#[derive(Clone, Copy)]
struct Handover {
    offset: usize,
}

pub(crate) struct RenderTargets<'a> {
    pub(crate) tracks: &'a mut ArenaRegistry<Arc<str>, PlayerTrack>,
    pub(crate) notification_tx: &'a mut HeapProd<PlayerNotification>,
}

pub(crate) struct RenderPass {
    scratch_bufs: [PcmBuf; Self::SCRATCH_BUF_COUNT],
}

impl RenderPass {
    /// Minimum stereo channel count for output processing.
    const MIN_STEREO: usize = 2;

    /// Number of scratch buffers for stereo processing.
    const SCRATCH_BUF_COUNT: usize = 4;

    pub(crate) fn new(pool: &PcmPool, max_frames: usize) -> Self {
        let scratch_bufs = std::array::from_fn(|_| {
            let mut buf = pool.get();
            buf.ensure_len(max_frames)
                .expect("scratch buffer exceeds PCM pool budget");
            buf.clear();
            buf
        });

        Self { scratch_bufs }
    }

    /// Render audio for all active tracks into the output buffers.
    pub(crate) fn render_audio(
        &mut self,
        mode: TrackRenderMode<'_>,
        targets: RenderTargets<'_>,
        buffers: &mut ProcBuffers,
        frames: usize,
        is_playing: bool,
    ) -> (bool, Option<(f64, f64)>, bool) {
        let mut playback_started = false;
        let mut leading_outcome_pos_dur: Option<(f64, f64)> = None;
        let multiple_tracks = targets
            .tracks
            .iter()
            .filter(|(_, track)| track.state().is_playing())
            .nth(1)
            .is_some();

        if buffers.outputs.len() < Self::MIN_STEREO {
            return (false, None, multiple_tracks);
        }

        for ch_buffer in buffers.outputs.iter_mut() {
            ch_buffer[..frames].fill(0.0);
        }

        for buf in &mut self.scratch_bufs {
            buf.ensure_len(frames)
                .expect("scratch buffer exceeds PCM pool budget");
        }

        let (left, right) = self.scratch_bufs.split_at_mut(Self::MIN_STEREO);
        let (read_buf0, read_buf1) = left.split_at_mut(1);
        let (mix_buf0, mix_buf1) = right.split_at_mut(1);
        let mut read_bufs = [&mut read_buf0[0][..frames], &mut read_buf1[0][..frames]];
        let mut mix_bufs = [&mut mix_buf0[0][..frames], &mut mix_buf1[0][..frames]];
        let tracks = targets.tracks;
        let notification_tx = targets.notification_tx;
        let join_offsets = activate_pending_joins(mode, tracks, notification_tx, is_playing);
        let arena_tracks: SmallVec<[(Index, TrackState); PlayerNodeProcessor::MAX_TRACKS]> =
            if is_playing {
                tracks
                    .iter()
                    .map(|(idx, track)| (idx, track.state()))
                    .collect()
            } else {
                SmallVec::new()
            };
        let active_tracks: SmallVec<[ActiveTrackEntry; PlayerNodeProcessor::MAX_TRACKS]> =
            arena_tracks
                .iter()
                .enumerate()
                .filter(|(_, (_, state))| state.is_playing())
                .map(|(arena_idx, (idx, state))| {
                    (arena_idx, *idx, state.is_leading(), join_offsets[arena_idx])
                })
                .collect();
        let mut active_arena_slots = [false; PlayerNodeProcessor::MAX_TRACKS];
        for (arena_idx, _, _, _) in &active_tracks {
            active_arena_slots[*arena_idx] = true;
        }
        let mut skip_tracks = [false; PlayerNodeProcessor::MAX_TRACKS];

        for (track_idx, (_arena_slot, track_handle, was_leading, start_offset)) in
            active_tracks.iter().enumerate()
        {
            if skip_tracks[track_idx] {
                continue;
            }

            for ch_buffer in &mut mix_bufs {
                ch_buffer.fill(0.0);
            }

            let start_offset = *start_offset;
            let mut read_outcome = {
                let Some(outcome) = tracks.get_by_index_mut(*track_handle).map(|track| {
                    track.render(
                        mode,
                        &mut read_bufs,
                        &mut mix_bufs,
                        start_offset..frames,
                        notification_tx,
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

                let mut handover = initial_handover(&read_outcome, start_offset);

                for (next_idx, (_, next_handle, next_is_leading, next_start)) in
                    active_tracks.iter().enumerate()
                {
                    let Some(handoff) = handover else {
                        break;
                    };
                    let offset = handoff.offset.max(*next_start);
                    if next_idx == track_idx || skip_tracks[next_idx] || !*next_is_leading {
                        continue;
                    }
                    if offset >= frames {
                        break;
                    }

                    let Some(outcome) = tracks.get_by_index_mut(*next_handle).map(|track| {
                        track.render(
                            mode,
                            &mut read_bufs,
                            &mut mix_bufs,
                            offset..frames,
                            notification_tx,
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
                        arena_tracks.iter().enumerate()
                    {
                        if *next_state != TrackState::Preloading
                            || active_arena_slots[next_arena_idx]
                        {
                            continue;
                        }

                        let Some(next_track) = tracks.get_by_index_mut(*next_handle) else {
                            continue;
                        };
                        next_track.play();
                        next_track.render(
                            mode,
                            &mut read_bufs,
                            &mut mix_bufs,
                            offset..frames,
                            notification_tx,
                        );
                        break;
                    }
                }
            }

            for (out_ch, mix_ch) in buffers.outputs.iter_mut().zip(mix_bufs.iter()) {
                out_ch
                    .iter_mut()
                    .take(frames)
                    .zip(mix_ch.iter())
                    .for_each(|(out_sample, &mix_sample)| *out_sample += mix_sample);
            }
        }

        (playback_started, leading_outcome_pos_dur, multiple_tracks)
    }

    pub(crate) fn resize(&mut self, max_frames: usize) {
        for buf in &mut self.scratch_bufs {
            buf.ensure_len(max_frames)
                .expect("scratch buffer exceeds PCM pool budget");
            buf.clear();
        }
    }
}

fn activate_pending_joins(
    mode: TrackRenderMode<'_>,
    tracks: &mut ArenaRegistry<Arc<str>, PlayerTrack>,
    notification_tx: &mut HeapProd<PlayerNotification>,
    is_playing: bool,
) -> [usize; PlayerNodeProcessor::MAX_TRACKS] {
    let mut offsets = [0; PlayerNodeProcessor::MAX_TRACKS];
    if !is_playing {
        return offsets;
    }
    let TrackRenderMode::Session(context) = mode else {
        return offsets;
    };
    for (arena_slot, (_, track)) in tracks.iter_mut().enumerate() {
        match track.activate_pending_join(context) {
            Ok(Some(offset)) => offsets[arena_slot] = offset,
            Ok(None) => {}
            Err(()) => track.handle_failed_end(notification_tx),
        }
    }
    offsets
}

fn initial_handover(read_outcome: &TrackReadOutcome, start_offset: usize) -> Option<Handover> {
    match read_outcome {
        TrackReadOutcome::Partial { frames, .. } => Some(Handover {
            offset: start_offset.saturating_add(*frames),
        }),
        TrackReadOutcome::Eof | TrackReadOutcome::Failed => Some(Handover {
            offset: start_offset,
        }),
        TrackReadOutcome::Full { .. } => None,
    }
}

fn next_handover(read_outcome: &TrackReadOutcome, offset: usize) -> Option<Handover> {
    match read_outcome {
        TrackReadOutcome::Full { .. } => None,
        TrackReadOutcome::Partial { frames, .. } => Some(Handover {
            offset: offset.saturating_add(*frames),
        }),
        TrackReadOutcome::Eof | TrackReadOutcome::Failed => Some(Handover { offset }),
    }
}

fn outcome_position_duration(outcome: &TrackReadOutcome) -> Option<(f64, f64)> {
    match *outcome {
        TrackReadOutcome::Full {
            position, duration, ..
        } => Some((position, duration)),
        TrackReadOutcome::Partial { duration, .. } => Some((duration, duration)),
        TrackReadOutcome::Eof | TrackReadOutcome::Failed => None,
    }
}

pub(super) fn eviction_priority(state: TrackState) -> u8 {
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
