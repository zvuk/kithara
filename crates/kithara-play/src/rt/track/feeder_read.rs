use std::{num::NonZeroUsize, ops::Range};

use kithara_audio::RevisionFloorStatus;
use kithara_events::TrackId;
use kithara_test_macros as kithara;
use kithara_warp::RenderContext;
use num_traits::ToPrimitive;

use super::feeder::{PlayerResource, ReadOutcome, activation_prefix, combine_reads};
use crate::bridge::RtMetrics;

#[derive(Clone, Copy)]
enum ActivationTailStatus {
    Disabled,
    Ready,
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ScheduledSeekPresentation {
    NoRequest,
    Presented(crate::bridge::ScheduledSeekDisposition),
    Superseded,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PreparedLaunchReadiness {
    NotReady,
    Ready { prefix_frames: usize },
}

impl PlayerResource {
    pub(crate) fn prepared_launch_readiness(
        &mut self,
        context: &RenderContext,
        frames: usize,
    ) -> PreparedLaunchReadiness {
        let Some(crate::rt::track::feeder::ScheduledSeekRecord {
            decoder_epoch: epoch,
            disposition: crate::bridge::ScheduledSeekDisposition::PreparedLaunch(identity),
            armed: true,
            ..
        }) = self.scheduled_seek
        else {
            return PreparedLaunchReadiness::NotReady;
        };
        let activation_block = identity.activation >= context.output_frames().start
            && identity.activation < context.output_frames().end;
        if activation_block {
            let observed = self.resource.get().render_activation();
            kithara::probe_event!(
                prepared_launch_readiness_checked,
                expected_activation = i64::from(identity.activation),
                observed_present = observed.is_some(),
                observed_activation = observed.map_or(0, |activation| i64::from(activation.output)),
                observed_revision = observed.map_or(0, |activation| activation.revision),
                ready = false
            );
        }
        let Some(activation) = self.resource.get().render_activation() else {
            return PreparedLaunchReadiness::NotReady;
        };
        let Some(revision) = kithara_signal::pack_render_revision(0, u64::from(identity.warp_map))
        else {
            self.scheduled_seek = None;
            return PreparedLaunchReadiness::NotReady;
        };
        if activation.output != identity.activation || activation.revision != revision {
            self.scheduled_seek = None;
            return PreparedLaunchReadiness::NotReady;
        }
        let output = activation.output;
        if output < context.output_frames().start {
            self.scheduled_seek = None;
            return PreparedLaunchReadiness::NotReady;
        }
        if self.resource.get_mut().present_seek(epoch)
            == kithara_audio::SeekPresentation::Superseded
        {
            self.scheduled_seek = None;
            return PreparedLaunchReadiness::NotReady;
        }
        self.resource.get().publish_render_preparation(context);
        if output >= context.output_frames().end {
            return PreparedLaunchReadiness::NotReady;
        }
        let Some(prefix_frames) = activation_prefix(context, activation) else {
            return PreparedLaunchReadiness::NotReady;
        };
        let suffix = frames.saturating_sub(prefix_frames);
        let Some(required) = NonZeroUsize::new(suffix) else {
            return PreparedLaunchReadiness::NotReady;
        };
        let underlying_status = self.resource.get_mut().sync_render_revision(
            activation.revision,
            required,
            self.last_source_end,
        );
        if underlying_status == RevisionFloorStatus::WaitingForReplacement {
            return PreparedLaunchReadiness::NotReady;
        }
        if self.sync_render_revision(activation, required)
            == RevisionFloorStatus::WaitingForReplacement
        {
            return PreparedLaunchReadiness::NotReady;
        }
        let ScheduledSeekPresentation::Presented(_) = self.present_scheduled_seek() else {
            return PreparedLaunchReadiness::NotReady;
        };
        if self.sync_render_revision(activation, required)
            == RevisionFloorStatus::WaitingForReplacement
        {
            return PreparedLaunchReadiness::NotReady;
        }
        if activation_block {
            kithara::probe_event!(
                prepared_launch_readiness_checked,
                expected_activation = i64::from(identity.activation),
                observed_present = true,
                observed_activation = i64::from(activation.output),
                observed_revision = activation.revision,
                ready = true
            );
        }
        PreparedLaunchReadiness::Ready { prefix_frames }
    }

    /// Read audio frames into the output buffers for the given range.
    ///
    /// Fills internal scratch buffers from the underlying resource as needed,
    /// then copies the requested frames into `output`. Shifts any remaining
    /// data to the front of the scratch buffers.
    ///
    /// When the underlying reader temporarily returns zero frames without EOF
    /// (for example, while an async seek is still settling), this method
    /// zero-fills the requested range and reports [`ReadOutcome::Full`].
    /// That silence is not a terminal condition and must not trigger track
    /// advancement.
    pub fn read(
        &mut self,
        output: &mut [&mut [f32]],
        range: Range<usize>,
        metrics: &RtMetrics,
    ) -> ReadOutcome {
        self.read_with_context(None, None, output, range, metrics).0
    }

    /// Reads `range` for the Host callback described by `context`.
    ///
    /// An activation inside the callback splits it: the prefix plays current
    /// PCM, and a replacement that is not ready yet leaves the suffix on
    /// current PCM instead of rereading the consumed prefix.
    pub(crate) fn read_with_context(
        &mut self,
        context: Option<&RenderContext>,
        track_id: Option<TrackId>,
        output: &mut [&mut [f32]],
        range: Range<usize>,
        metrics: &RtMetrics,
    ) -> (ReadOutcome, u64) {
        let Some(context) = context else {
            return self.read_current(None, track_id, output, range, metrics);
        };
        let Some(activation) = self.resource.get().render_activation() else {
            return self.read_current(Some(context), track_id, output, range, metrics);
        };
        if activation.revision <= self.render_revision_floor {
            return self.read_current(Some(context), track_id, output, range, metrics);
        }
        let Some(prefix_frames) = activation_prefix(context, activation) else {
            self.fill_scratch(self.channel_buffers[0].len(), metrics);
            return self.read_current(Some(context), track_id, output, range, metrics);
        };
        if matches!(
            self.scheduled_seek,
            Some(crate::rt::track::feeder::ScheduledSeekRecord {
                disposition: crate::bridge::ScheduledSeekDisposition::PreparedLaunch(_),
                ..
            })
        ) {
            return self.read_current(Some(context), track_id, output, range, metrics);
        }
        let frames_to_read = range.end - range.start;
        if prefix_frames == 0 {
            let tail = self.prepare_activation_tail(metrics);
            if let Some(required) = NonZeroUsize::new(frames_to_read) {
                match self.sync_render_revision(activation, required) {
                    RevisionFloorStatus::WaitingForReplacement => {
                        return self.read_current(Some(context), track_id, output, range, metrics);
                    }
                    RevisionFloorStatus::ReadyForSeekPresentation => {
                        if matches!(
                            self.scheduled_seek,
                            Some(crate::rt::track::feeder::ScheduledSeekRecord {
                                disposition: crate::bridge::ScheduledSeekDisposition::SeekOnly { .. },
                                ..
                            })
                        ) {
                            let _ = self.present_scheduled_seek();
                        }
                        if self.sync_render_revision(activation, required)
                            == RevisionFloorStatus::WaitingForReplacement
                        {
                            return self.read_current(
                                Some(context),
                                track_id,
                                output,
                                range,
                                metrics,
                            );
                        }
                    }
                    RevisionFloorStatus::Current | RevisionFloorStatus::Switched => {}
                }
            }
            self.arm_activation_blend(tail);
            return self.read_current(Some(context), track_id, output, range, metrics);
        }

        let prefix_end = range.start.saturating_add(prefix_frames);
        let Some(prefix_context) = context.for_output_range(0..prefix_frames) else {
            return (ReadOutcome::Failed, 0);
        };
        let (prefix, prefix_source_frames) = self.read_current(
            Some(&prefix_context),
            track_id,
            output,
            range.start..prefix_end,
            metrics,
        );
        let ReadOutcome::Full {
            frames: copied_prefix,
        } = prefix
        else {
            return (prefix, prefix_source_frames);
        };
        if copied_prefix != prefix_frames {
            return (prefix, prefix_source_frames);
        }

        let suffix_frames = frames_to_read - prefix_frames;
        let tail = self.prepare_activation_tail(metrics);
        let required = NonZeroUsize::new(suffix_frames).expect("activation suffix is non-zero");
        let replaced = match self.sync_render_revision(activation, required) {
            RevisionFloorStatus::WaitingForReplacement => false,
            RevisionFloorStatus::ReadyForSeekPresentation => {
                if matches!(
                    self.scheduled_seek,
                    Some(crate::rt::track::feeder::ScheduledSeekRecord {
                        disposition: crate::bridge::ScheduledSeekDisposition::SeekOnly { .. },
                        ..
                    })
                ) {
                    let _ = self.present_scheduled_seek();
                }
                self.sync_render_revision(activation, required)
                    != RevisionFloorStatus::WaitingForReplacement
            }
            RevisionFloorStatus::Current | RevisionFloorStatus::Switched => true,
        };
        if replaced {
            self.arm_activation_blend(tail);
        }
        let Some(suffix_context) = context.for_output_range(prefix_frames..frames_to_read) else {
            return (ReadOutcome::Failed, prefix_source_frames);
        };
        let (suffix, suffix_source_frames) = self.read_current(
            Some(&suffix_context),
            track_id,
            output,
            prefix_end..range.end,
            metrics,
        );
        combine_reads(
            prefix_frames,
            prefix_source_frames,
            suffix,
            suffix_source_frames,
        )
    }

    fn read_current(
        &mut self,
        context: Option<&RenderContext>,
        track_id: Option<TrackId>,
        output: &mut [&mut [f32]],
        range: Range<usize>,
        metrics: &RtMetrics,
    ) -> (ReadOutcome, u64) {
        let frames_to_read = range.len();
        let mut eof_reached = self.fill_scratch(frames_to_read, metrics);

        if self.write_len == 0 && self.failed && !self.eof_seen {
            for ch in output.iter_mut() {
                ch[range.clone()].fill(0.0);
            }
            return (ReadOutcome::Failed, 0);
        }

        if self.write_len > 0 {
            let frames_to_write = frames_to_read.min(self.write_len);
            let tail_size = self.write_len - frames_to_write;

            if output.len() >= Self::STEREO_CHANNELS {
                output[0][range.start..range.start + frames_to_write]
                    .copy_from_slice(&self.channel_buffers[0][..frames_to_write]);
                output[1][range.start..range.start + frames_to_write]
                    .copy_from_slice(&self.channel_buffers[1][..frames_to_write]);
                self.apply_activation_blend(output, range.start, frames_to_write);
            }

            let Some(source_frames) = self.consume_source(frames_to_write, range.start, context)
            else {
                metrics.record_decode_error();
                self.failed = true;
                self.last_source_end = None;
                self.source_spans.clear();
                self.write_len = 0;
                self.write_pos = 0;
                return (ReadOutcome::Failed, 0);
            };

            if tail_size > 0 {
                self.channel_buffers[0]
                    .copy_within(frames_to_write..frames_to_write + tail_size, 0);
                self.channel_buffers[1]
                    .copy_within(frames_to_write..frames_to_write + tail_size, 0);
            }

            self.write_len -= frames_to_write;
            self.write_pos = tail_size;

            if frames_to_write == frames_to_read {
                let target = self.prefetch_target(frames_to_read);
                eof_reached |= self.fill_scratch(target, metrics);
            }

            let outcome = if frames_to_write == frames_to_read {
                ReadOutcome::Full {
                    frames: frames_to_write,
                }
            } else if eof_reached {
                ReadOutcome::Partial {
                    frames: frames_to_write,
                }
            } else {
                self.fill_underrun(context, track_id, output, range, frames_to_write, metrics);
                ReadOutcome::Full {
                    frames: frames_to_write,
                }
            };
            (outcome, source_frames)
        } else if eof_reached {
            (ReadOutcome::Eof, 0)
        } else {
            self.fill_underrun(context, track_id, output, range, 0, metrics);
            (ReadOutcome::Full { frames: 0 }, 0)
        }
    }

    fn prepare_activation_tail(&mut self, metrics: &RtMetrics) -> ActivationTailStatus {
        let frames = self.activation_blend_frames;
        if self.activation_tail.is_none() {
            return ActivationTailStatus::Disabled;
        }
        self.fill_scratch(frames, metrics);
        if self.write_len < frames {
            return ActivationTailStatus::Unavailable;
        }
        let Some(activation_tail) = self.activation_tail.as_mut() else {
            return ActivationTailStatus::Disabled;
        };
        for (tail, buffered) in activation_tail.iter_mut().zip(&self.channel_buffers) {
            tail[..frames].copy_from_slice(&buffered[..frames]);
        }
        ActivationTailStatus::Ready
    }

    fn arm_activation_blend(&mut self, status: ActivationTailStatus) {
        self.activation_blend_pos = match status {
            ActivationTailStatus::Ready => 0,
            ActivationTailStatus::Disabled | ActivationTailStatus::Unavailable => {
                self.activation_blend_frames
            }
        };
    }

    fn apply_activation_blend(
        &mut self,
        output: &mut [&mut [f32]],
        output_start: usize,
        frames: usize,
    ) {
        let Some(activation_tail) = self.activation_tail.as_ref() else {
            return;
        };
        let remaining = self
            .activation_blend_frames
            .saturating_sub(self.activation_blend_pos);
        let blended = frames.min(remaining);
        let denominator = self.activation_blend_frames.to_f32().unwrap_or(f32::MAX);
        for offset in 0..blended {
            let frame = self.activation_blend_pos + offset;
            let incoming_gain = frame.to_f32().unwrap_or(f32::MAX) / denominator;
            let outgoing_gain = 1.0 - incoming_gain;
            for (incoming, outgoing) in output
                .iter_mut()
                .zip(activation_tail)
                .take(Self::STEREO_CHANNELS)
            {
                let incoming = &mut incoming[output_start + offset];
                *incoming = outgoing[frame].mul_add(outgoing_gain, *incoming * incoming_gain);
            }
        }
        self.activation_blend_pos += blended;
    }
}
