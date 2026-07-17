use std::{num::NonZeroU32, ops::Range};

use kithara_audio::ServiceClass;
use kithara_bufpool::{PcmBuf, PcmPool};
use kithara_platform::{maybe_send::WasmSend, sync::Arc, time::Duration};
use tracing::warn;

#[cfg_attr(target_arch = "wasm32", path = "../platform/browser.rs")]
#[cfg_attr(not(target_arch = "wasm32"), path = "native.rs")]
mod platform;

#[rustfmt::skip]
use crate::resource::Resource;

pub(crate) use platform::PreparedElasticRenderer;

/// RT-safe wrapper for standalone and bound elastic playback.
/// Standalone resources own pooled scratch buffers; elastic resources use prepared renderer
/// storage.
pub struct PlayerResource {
    src: Arc<str>,
    resource: WasmSend<Resource>,
    channel_buffers: Option<[PcmBuf; Self::STEREO_CHANNELS]>,
    eof_seen: bool,
    failed: bool,
    write_len: usize,
    write_pos: usize,
    elastic_renderer: Option<PreparedElasticRenderer>,
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
    delegate::delegate! {
        to self.resource.get() {
            /// Total duration in seconds. Returns 0.0 if unknown.
            #[must_use]
            #[expr($.map_or(0.0, |d| d.as_secs_f64()))]
            pub fn duration(&self) -> f64;
            /// Set the target sample rate of the audio host.
            pub(crate) fn set_host_sample_rate(&self, sample_rate: NonZeroU32);
            /// Set the playback rate for the active stretch controls.
            pub(crate) fn set_playback_rate(&self, rate: f32);
            /// Set the transport pitch-bend multiplier.
            pub(crate) fn set_transport_bend(&self, bend: f32);
        }
    }

    /// Buffer duration divisor: `sample_rate` / `BUFFER_DURATION_DIVISOR` gives ~200ms of frames.
    const BUFFER_DURATION_DIVISOR: usize = 5;

    /// Number of stereo output channels.
    const STEREO_CHANNELS: usize = 2;

    /// Create a new `PlayerResource` wrapping the given resource.
    ///
    /// Allocates two channel scratch buffers from the given PCM pool,
    /// sized to `sample_rate / 5` frames (200ms worth of audio).
    #[must_use]
    pub fn new(resource: Resource, src: Arc<str>, pool: &PcmPool) -> Self {
        let spec = resource.spec();
        let channels = spec.channels as usize;
        let buffer_len = (spec.sample_rate.get() as usize / Self::BUFFER_DURATION_DIVISOR)
            * channels.max(Self::STEREO_CHANNELS);

        let channel_buffers = std::array::from_fn(|_| {
            pool.get_with(|b: &mut Vec<f32>| {
                let cap = b.capacity();
                if cap < buffer_len {
                    b.reserve(buffer_len - cap);
                }
                b.resize(buffer_len, 0.0);
            })
        });

        Self {
            resource: WasmSend::new(resource),
            channel_buffers: Some(channel_buffers),
            src,
            write_len: 0,
            write_pos: 0,
            eof_seen: false,
            failed: false,
            elastic_renderer: None,
        }
    }

    pub(crate) fn release(self) -> (Resource, Option<PreparedElasticRenderer>) {
        let Self {
            resource,
            elastic_renderer,
            ..
        } = self;
        (resource.into_inner(), elastic_renderer)
    }

    pub(crate) fn set_service_class(&mut self, class: ServiceClass) {
        if let Some(renderer) = self.elastic_renderer.as_mut() {
            renderer.set_service_class(class);
            return;
        }
        self.resource.get().set_service_class(class);
    }

    /// Source identifier attached to this resource.
    #[must_use]
    pub fn src(&self) -> &Arc<str> {
        &self.src
    }

    /// Decoded-ahead frontier in seconds: how much content has been decoded
    /// and is ready to play (always `>=` the served playback position).
    #[must_use]
    pub fn decoded_frontier(&self) -> f64 {
        if let Some(renderer) = self.elastic_renderer.as_ref() {
            return renderer.decoded_frontier();
        }
        self.resource.get().decoded_frontier().as_secs_f64()
    }

    fn fill_scratch(&mut self, target_frames: usize) -> bool {
        let Some(channel_buffers) = self.channel_buffers.as_mut() else {
            self.failed = true;
            return false;
        };
        let mut eof_reached = self.eof_seen;

        while target_frames > self.write_len && !eof_reached {
            let avail = channel_buffers[0].len() - self.write_pos;
            if avail == 0 {
                break;
            }

            let (left_buf, right_buf) = channel_buffers.split_at_mut(1);
            let left = &mut left_buf[0][self.write_pos..self.write_pos + avail];
            let right = &mut right_buf[0][self.write_pos..self.write_pos + avail];
            let mut planar: [&mut [f32]; Self::STEREO_CHANNELS] = [left, right];

            let n = match self.resource.get_mut().read_planar(&mut planar) {
                Ok(kithara_audio::ReadOutcome::Frames { count, .. }) => count.get(),
                Ok(kithara_audio::ReadOutcome::Pending { .. }) => 0,
                Ok(kithara_audio::ReadOutcome::Eof { .. }) => {
                    self.eof_seen = true;
                    eof_reached = true;
                    0
                }
                Err(err) => {
                    warn!(src = %self.src, error = %err, "PlayerResource: decode error");
                    self.failed = true;
                    0
                }
            };
            if n == 0 {
                break;
            }
            self.write_len += n;
            self.write_pos += n;
        }

        eof_reached
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
    pub fn read(&mut self, output: &mut [&mut [f32]], range: Range<usize>) -> ReadOutcome {
        if self.channel_buffers.is_none() {
            for channel in output.iter_mut() {
                channel[..range.len()].fill(0.0);
            }
            return ReadOutcome::Failed;
        }
        let frames_to_read = range.end - range.start;
        let mut eof_reached = self.fill_scratch(frames_to_read);

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
            let Some(channel_buffers) = self.channel_buffers.as_mut() else {
                for channel in output.iter_mut() {
                    channel[..range.len()].fill(0.0);
                }
                return ReadOutcome::Failed;
            };

            if output.len() >= Self::STEREO_CHANNELS {
                output[0][..frames_to_write]
                    .copy_from_slice(&channel_buffers[0][..frames_to_write]);
                output[1][..frames_to_write]
                    .copy_from_slice(&channel_buffers[1][..frames_to_write]);
            }

            if tail_size > 0 {
                channel_buffers[0].copy_within(frames_to_write..frames_to_write + tail_size, 0);
                channel_buffers[1].copy_within(frames_to_write..frames_to_write + tail_size, 0);
            }

            self.write_len -= frames_to_write;
            self.write_pos = tail_size;

            if frames_to_write == frames_to_read {
                eof_reached |= self.fill_scratch(frames_to_read);
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
            let range_len = range.len();
            for ch in output.iter_mut() {
                ch[..range_len].fill(0.0);
            }
            ReadOutcome::Full { frames: 0 }
        }
    }

    /// Seeks standalone playback and clears its scratch state.
    /// Returns `false` for bound elastic playback or source seek failure.
    #[must_use]
    pub fn seek(&mut self, seconds: f64) -> bool {
        if self.elastic_renderer.is_some() {
            return false;
        }
        let position = Duration::from_secs_f64(seconds);
        match self.resource.get_mut().seek(position) {
            Ok(_) => {
                self.reset_read_state();
                true
            }
            Err(err) => {
                warn!("failed to seek: {err}");
                false
            }
        }
    }

    fn reset_read_state(&mut self) {
        self.write_len = 0;
        self.write_pos = 0;
        self.eof_seen = false;
        self.failed = false;
    }
}
