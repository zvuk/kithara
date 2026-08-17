use std::{num::NonZeroU32, ops::Range};

use kithara_audio::{PresentationAdvance, PresentationPoint, ServiceClass};
use kithara_bufpool::{PcmBuf, PcmPool};
use kithara_decode::Frames;
use kithara_platform::{maybe_send::WasmSend, sync::Arc};

#[rustfmt::skip]
use crate::resource::Resource;
use crate::bridge::RtMetrics;

/// RT-safe resource wrapper with internal scratch buffers.
///
/// Wraps a [`Resource`] and maintains per-channel scratch buffers
/// that are filled from the underlying `PcmReader`. The audio thread
/// reads from these buffers, avoiding direct interaction with the
/// potentially-blocking decoder on every callback.
#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub struct PlayerResource {
    #[field(get, deref = false)]
    src: Arc<str>,
    resource: WasmSend<Resource>,
    channel_buffers: [PcmBuf; Self::STEREO_CHANNELS],
    eof_seen: bool,
    failed: bool,
    write_len: usize,
    write_pos: usize,
    buffered_presentation_advance: Option<PresentationAdvance>,
    read_presentation_advance: Option<PresentationAdvance>,
}

/// Result of a bounded audio-thread read from [`PlayerResource`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadOutcome {
    /// The requested range was filled completely.
    ///
    /// `frames` counts real PCM frames copied out of the wrapped reader or
    /// scratch buffer. The remainder may be zero-filled during a non-terminal
    /// underrun and must not advance playback position.
    Full { frames: usize },
    /// A strict prefix of the requested range was written.
    ///
    /// The payload is the number of written frames. This outcome is reserved
    /// for natural EOF inside the requested block; the next read must return
    /// [`ReadOutcome::Eof`].
    Partial { frames: usize },
    /// The resource was already drained and nothing was written.
    Eof,
    /// The underlying decoder/source reported a non-recoverable error
    /// mid-stream. Distinct from [`Eof`](Self::Eof): the track did NOT
    /// reach its natural end — surface this as a track-failed signal
    /// upstream instead of letting the queue auto-advance as if the
    /// track played out.
    Failed,
}

impl PlayerResource {
    /// Buffer duration divisor: `sample_rate` / `BUFFER_DURATION_DIVISOR` gives ~200ms of frames.
    const BUFFER_DURATION_DIVISOR: usize = 5;

    /// Number of stereo output channels.
    const STEREO_CHANNELS: usize = 2;

    const fn scratch_frames(sample_rate: u32) -> Frames {
        Frames::new(sample_rate as usize / Self::BUFFER_DURATION_DIVISOR)
    }

    /// Create a new `PlayerResource` wrapping the given resource.
    ///
    /// Allocates two per-channel scratch buffers from the given PCM pool, each holding
    /// [`Self::scratch_frames`] frames.
    #[must_use]
    pub fn new(resource: Resource, src: Arc<str>, pool: &PcmPool) -> Self {
        let buffer_frames = Self::scratch_frames(resource.spec().sample_rate.get()).get();

        let channel_buffers = std::array::from_fn(|_| {
            pool.get_with(|b: &mut Vec<f32>| {
                let cap = b.capacity();
                if cap < buffer_frames {
                    b.reserve(buffer_frames - cap);
                }
                b.resize(buffer_frames, 0.0);
            })
        });

        Self {
            channel_buffers,
            src,
            resource: WasmSend::new(resource),
            write_len: 0,
            write_pos: 0,
            eof_seen: false,
            failed: false,
            buffered_presentation_advance: None,
            read_presentation_advance: None,
        }
    }

    /// Cached span in seconds: how much of the source is on disk and needs no
    /// further network.
    #[must_use]
    pub fn cached_span(&self) -> f64 {
        self.resource.get().cached_span().as_secs_f64()
    }

    /// Decoded-ahead frontier in seconds: how much content has been decoded
    /// and is ready to play (always `>=` the served playback position).
    #[must_use]
    pub fn decoded_frontier(&self) -> f64 {
        self.resource.get().decoded_frontier().as_secs_f64()
    }

    fn fill_scratch(&mut self, target_frames: usize, metrics: &RtMetrics) -> bool {
        let mut eof_reached = self.eof_seen;

        while target_frames > self.write_len && !eof_reached {
            let deficit = target_frames - self.write_len;
            let avail = (self.channel_buffers[0].len() - self.write_pos).min(deficit);
            if avail == 0 {
                break;
            }

            let write_len_before = self.write_len;
            let read_outcome = {
                let channel_buffers = &mut self.channel_buffers;
                let (left_buf, right_buf) = channel_buffers.split_at_mut(1);
                let left = &mut left_buf[0][self.write_pos..self.write_pos + avail];
                let right = &mut right_buf[0][self.write_pos..self.write_pos + avail];
                let mut planar: [&mut [f32]; Self::STEREO_CHANNELS] = [left, right];
                self.resource.get_mut().read_planar(&mut planar)
            };
            let advance = self.resource.get_mut().take_presentation_advance();
            let n = match read_outcome {
                Ok(kithara_audio::ReadOutcome::Frames { count, .. }) => count.get(),
                Ok(kithara_audio::ReadOutcome::Pending { .. }) => 0,
                Ok(kithara_audio::ReadOutcome::Eof { .. }) => {
                    self.eof_seen = true;
                    eof_reached = true;
                    0
                }
                Err(_) => {
                    metrics.record_decode_error();
                    self.failed = true;
                    0
                }
            };
            if n == 0 {
                if advance.is_some() {
                    self.buffered_presentation_advance = None;
                }
                break;
            }
            self.record_presentation_advance(write_len_before, n, advance);
            self.write_len += n;
            self.write_pos += n;
        }

        eof_reached
    }

    fn record_presentation_advance(
        &mut self,
        write_len_before: usize,
        frames_read: usize,
        advance: Option<PresentationAdvance>,
    ) {
        let Some(advance) = advance else {
            return;
        };
        let read_offset = advance.read_offset_frames();
        if read_offset > frames_read {
            self.buffered_presentation_advance = None;
            return;
        }
        self.buffered_presentation_advance = write_len_before
            .checked_add(read_offset)
            .map(|offset| PresentationAdvance::new(advance.point(), offset));
    }

    fn take_consumed_presentation_advance(&mut self, frames: usize) -> Option<PresentationAdvance> {
        let advance = self.buffered_presentation_advance?;
        let offset = advance.read_offset_frames();
        if offset <= frames {
            self.buffered_presentation_advance = None;
            return Some(advance);
        }
        self.buffered_presentation_advance =
            Some(PresentationAdvance::new(advance.point(), offset - frames));
        None
    }

    /// Remaining buffered frames when the wrapped reader has reached EOF.
    ///
    /// `Some(0)` means the current read drained the last buffered frame exactly;
    /// the next read will return [`ReadOutcome::Eof`].
    #[must_use]
    pub fn frames_until_eof(&self) -> Option<usize> {
        self.eof_seen.then_some(self.write_len)
    }

    /// Read PCM frames into the output buffers for the given range.
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
        self.read_presentation_advance = None;
        let frames_to_read = range.end - range.start;
        let mut eof_reached = self.fill_scratch(frames_to_read, metrics);

        if self.write_len == 0 && self.failed && !self.eof_seen {
            let range_len = range.len();
            for ch in output.iter_mut() {
                ch[..range_len].fill(0.0);
            }
            return ReadOutcome::Failed;
        }

        if self.write_len > 0 {
            let frames_to_write = frames_to_read.min(self.write_len);
            let tail_size = self.write_len - frames_to_write;

            if output.len() >= Self::STEREO_CHANNELS {
                output[0][..frames_to_write]
                    .copy_from_slice(&self.channel_buffers[0][..frames_to_write]);
                output[1][..frames_to_write]
                    .copy_from_slice(&self.channel_buffers[1][..frames_to_write]);
            }

            if tail_size > 0 {
                self.channel_buffers[0]
                    .copy_within(frames_to_write..frames_to_write + tail_size, 0);
                self.channel_buffers[1]
                    .copy_within(frames_to_write..frames_to_write + tail_size, 0);
            }

            self.write_len -= frames_to_write;
            self.write_pos = tail_size;
            self.read_presentation_advance =
                self.take_consumed_presentation_advance(frames_to_write);

            if frames_to_write == frames_to_read {
                eof_reached |= self.fill_scratch(frames_to_read, metrics);
            }

            if frames_to_write == frames_to_read {
                ReadOutcome::Full {
                    frames: frames_to_write,
                }
            } else if eof_reached {
                ReadOutcome::Partial {
                    frames: frames_to_write,
                }
            } else {
                metrics.record_underrun();
                for ch in output.iter_mut() {
                    ch[frames_to_write..frames_to_read].fill(0.0);
                }
                ReadOutcome::Full {
                    frames: frames_to_write,
                }
            }
        } else if eof_reached {
            ReadOutcome::Eof
        } else {
            metrics.record_underrun();
            let range_len = range.len();
            for ch in output.iter_mut() {
                ch[..range_len].fill(0.0);
            }
            ReadOutcome::Full { frames: 0 }
        }
    }

    /// Drop everything buffered ahead of a seek the control thread began. Lock-free: the reader
    /// picks up the epoch itself via `sync_seek`.
    pub fn reset_for_seek(&mut self) {
        self.resource.get_mut().sync_seek();
        self.write_len = 0;
        self.write_pos = 0;
        self.eof_seen = false;
        self.failed = false;
        self.buffered_presentation_advance = None;
        self.read_presentation_advance = None;
    }

    /// Control-plane handle used to begin a seek off the audio thread.
    #[must_use]
    pub fn seek_handle(&self) -> Option<Arc<dyn kithara_audio::SeekBegin>> {
        self.resource.get().seek_handle()
    }

    pub(super) fn take_presentation_advance(&mut self) -> Option<PresentationAdvance> {
        self.read_presentation_advance.take()
    }

    delegate::delegate! {
        to self.resource.get() {
            /// Total duration in seconds. Returns 0.0 if unknown.
            #[must_use]
            #[expr($.map_or(0.0, |d| d.as_secs_f64()))]
            pub fn duration(&self) -> f64;
            /// Latest coherent producer-side presentation endpoint.
            #[must_use]
            pub(crate) fn presentation_point(&self) -> Option<PresentationPoint>;
            /// Set the target sample rate of the audio host.
            pub(crate) fn set_host_sample_rate(&self, sample_rate: NonZeroU32);
            /// Set the playback rate for the active stretch controls.
            pub(crate) fn set_playback_rate(&self, rate: f32);
            /// Update the scheduling priority hint for the shared worker.
            pub(crate) fn set_service_class(&self, class: ServiceClass);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;

    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    #[case(44_100, 8_820)]
    #[case(48_000, 9_600)]
    #[case(96_000, 19_200)]
    fn scratch_holds_200ms_of_frames(#[case] sample_rate: u32, #[case] expected: usize) {
        assert_eq!(
            PlayerResource::scratch_frames(sample_rate),
            Frames::new(expected)
        );
    }

    #[kithara::test]
    fn an_interleaved_length_is_not_a_frame_count() {
        const STEREO: NonZeroUsize = NonZeroUsize::new(2).expect("2 is non-zero");

        let frames = PlayerResource::scratch_frames(48_000);
        assert_eq!(frames.samples(STEREO).get(), frames.get() * 2);
    }
}
