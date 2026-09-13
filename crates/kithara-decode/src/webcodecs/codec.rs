use std::sync::atomic::{AtomicU64, Ordering};

use kithara_bufpool::{HasPool, PoolRegion, SampleBuffer};
use kithara_platform::{
    sync::{
        Arc,
        mpsc::{self, RecvTimeoutError, TryRecvError},
    },
    time::{Duration, Instant},
};
use kithara_signal::AudioSpec;
use kithara_stream::AudioCodec;

use super::protocol::{HostCmd, HostOut};
use crate::{
    codec::FrameCodec,
    demuxer::TrackInfo,
    error::{DecodeError, DecodeResult},
    types::{DecoderTrackInfo, checked_audio_spec},
};

struct Consts;

static NEXT_DECODER_ID: AtomicU64 = AtomicU64::new(1);

impl Consts {
    const DRAIN_TIMEOUT: Duration = Duration::from_millis(250);
    const FLAC_DESCRIPTION_LEN: usize = 42;
    const FLAC_STREAMINFO_LEN: u8 = 34;
    const OUTPUT_TIMEOUT: Duration = Duration::from_millis(10);
}

#[derive(Debug, thiserror::Error)]
enum WebCodecsError {
    #[error("WebCodecs backend failed: {detail}")]
    Backend { detail: String },
    #[error("WebCodecs host {channel} channel disconnected")]
    ChannelDisconnected { channel: &'static str },
    #[error("WebCodecs runtime was not initialized on the browser main thread")]
    RuntimeNotInitialized,
    #[error("WebCodecs flush timed out for generation {generation}")]
    FlushTimeout { generation: u64 },
    #[error(
        "WebCodecs PCM output length {samples} does not match {frames} frames and {channels} channels"
    )]
    OutputShape {
        samples: usize,
        frames: u32,
        channels: u16,
    },
}

struct CodecConfig {
    codec_string: &'static str,
    description: Option<Arc<[u8]>>,
    channels: u16,
    sample_rate: u32,
}

struct PcmOut {
    interleaved: SampleBuffer,
    channels: u16,
    frames: u32,
    sample_rate: u32,
    generation: u64,
    pts_us: u64,
}

pub(crate) struct WebCodecsCodec<S> {
    spec: AudioSpec,
    config: CodecConfig,
    track_info: DecoderTrackInfo,
    decoded_pts: Duration,
    pools: PoolRegion<S>,
    out: mpsc::Receiver<HostOut>,
    cmd: mpsc::Sender<HostCmd>,
    eof_draining: bool,
    eof_flushed: bool,
    decoder_id: u64,
    generation: u64,
}

impl<S> WebCodecsCodec<S>
where
    S: HasPool<u8>,
{
    fn drain_output(&mut self, out: &mut SampleBuffer) -> DecodeResult<u32> {
        let deadline = Instant::now() + Consts::DRAIN_TIMEOUT;
        loop {
            let output = match self.out.recv_timeout(deadline) {
                Ok(output) => output,
                Err(RecvTimeoutError::Timeout) => {
                    tracing::warn!(
                        codec = self.config.codec_string,
                        decoder_id = self.decoder_id,
                        generation = self.generation,
                        "timed out draining WebCodecs flush"
                    );
                    return Err(DecodeError::backend(WebCodecsError::FlushTimeout {
                        generation: self.generation,
                    }));
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(channel_disconnected("output"));
                }
                Err(_) => return Err(channel_disconnected("output")),
            };

            match output {
                HostOut::Pcm {
                    interleaved,
                    frames,
                    sample_rate,
                    channels,
                    pts_us,
                    generation,
                } if generation == self.generation => {
                    let pcm = PcmOut {
                        interleaved,
                        channels,
                        frames,
                        sample_rate,
                        generation,
                        pts_us,
                    };
                    return self.write_pcm(out, pcm);
                }
                HostOut::Flushed { generation } if generation == self.generation => {
                    self.eof_flushed = true;
                    out.clear();
                    return Ok(0);
                }
                HostOut::Error { detail, generation } if generation == self.generation => {
                    return Err(DecodeError::backend(WebCodecsError::Backend { detail }));
                }
                HostOut::Configured {
                    sample_rate,
                    channels,
                    generation,
                } if generation == self.generation => {
                    self.spec =
                        checked_audio_spec(channels, sample_rate, "webcodecs.output.sample_rate")?;
                }
                HostOut::Pcm { generation, .. }
                | HostOut::Configured { generation, .. }
                | HostOut::Flushed { generation }
                | HostOut::Error { generation, .. } => {
                    tracing::debug!(
                        output_generation = generation,
                        generation = self.generation,
                        "dropping stale WebCodecs output"
                    );
                }
            }
        }
    }

    pub(crate) fn open(
        track: &TrackInfo,
        gapless_enabled: bool,
        pools: PoolRegion<S>,
    ) -> DecodeResult<Self> {
        let config = codec_config(track)?;
        let spec = checked_audio_spec(
            track.channels,
            track.sample_rate,
            "webcodecs.track.sample_rate",
        )?;
        let cmd = super::probe::host_sender()
            .ok_or_else(|| DecodeError::backend(WebCodecsError::RuntimeNotInitialized))?;
        let decoder_id = NEXT_DECODER_ID.fetch_add(1, Ordering::Relaxed);
        let (reply_tx, out) = mpsc::channel();
        cmd.send(HostCmd::Open {
            reply_tx,
            id: decoder_id,
        })
        .map_err(|_| channel_disconnected("command"))?;
        let codec = Self {
            pools,
            decoder_id,
            cmd,
            out,
            spec,
            config,
            generation: 0,
            track_info: DecoderTrackInfo {
                gapless: if gapless_enabled { track.gapless } else { None },
                ..DecoderTrackInfo::default()
            },
            decoded_pts: Duration::ZERO,
            eof_draining: false,
            eof_flushed: false,
        };
        codec.send_configure()?;
        tracing::debug!(
            decoder_id,
            codec = codec.config.codec_string,
            generation = 0,
            "configured WebCodecs codec"
        );
        Ok(codec)
    }

    fn poll_output(&mut self, out: &mut SampleBuffer) -> DecodeResult<u32> {
        let first = match self
            .out
            .recv_timeout(Instant::now() + Consts::OUTPUT_TIMEOUT)
        {
            Ok(output) => output,
            Err(RecvTimeoutError::Timeout) => {
                out.clear();
                return Ok(0);
            }
            Err(RecvTimeoutError::Disconnected) => {
                return Err(channel_disconnected("output"));
            }
            Err(_) => return Err(channel_disconnected("output")),
        };

        let mut output = Some(first);
        loop {
            let current = match output.take() {
                Some(current) => current,
                None => match self.out.try_recv() {
                    Ok(current) => current,
                    Err(TryRecvError::Empty) => {
                        out.clear();
                        return Ok(0);
                    }
                    Err(TryRecvError::Disconnected) => {
                        return Err(channel_disconnected("output"));
                    }
                    Err(_) => return Err(channel_disconnected("output")),
                },
            };

            match current {
                HostOut::Pcm {
                    interleaved,
                    frames,
                    sample_rate,
                    channels,
                    pts_us,
                    generation,
                } if generation == self.generation => {
                    let pcm = PcmOut {
                        interleaved,
                        channels,
                        frames,
                        sample_rate,
                        generation,
                        pts_us,
                    };
                    return self.write_pcm(out, pcm);
                }
                HostOut::Configured {
                    sample_rate,
                    channels,
                    generation,
                } if generation == self.generation => {
                    self.spec =
                        checked_audio_spec(channels, sample_rate, "webcodecs.output.sample_rate")?;
                    out.clear();
                    return Ok(0);
                }
                HostOut::Error { detail, generation } if generation == self.generation => {
                    return Err(DecodeError::backend(WebCodecsError::Backend { detail }));
                }
                HostOut::Flushed { generation } if generation == self.generation => {
                    tracing::debug!(
                        generation,
                        "dropping completed WebCodecs flush outside EOF drain"
                    );
                }
                HostOut::Pcm { generation, .. }
                | HostOut::Configured { generation, .. }
                | HostOut::Flushed { generation }
                | HostOut::Error { generation, .. } => {
                    tracing::debug!(
                        output_generation = generation,
                        generation = self.generation,
                        "dropping stale WebCodecs output"
                    );
                }
            }
        }
    }

    fn write_pcm(&mut self, out: &mut SampleBuffer, pcm: PcmOut) -> DecodeResult<u32> {
        let PcmOut {
            interleaved,
            channels,
            frames,
            sample_rate,
            generation,
            pts_us,
        } = pcm;
        let expected = usize::try_from(frames)
            .ok()
            .and_then(|frames| frames.checked_mul(usize::from(channels)));
        if expected != Some(interleaved.len()) {
            return Err(DecodeError::backend(WebCodecsError::OutputShape {
                frames,
                channels,
                samples: interleaved.len(),
            }));
        }
        *out = interleaved;
        self.decoded_pts = Duration::from_micros(pts_us);
        tracing::debug!(
            generation,
            decoder_id = self.decoder_id,
            pts_us,
            sample_rate,
            channels,
            frames,
            "received WebCodecs PCM"
        );
        Ok(frames)
    }
}

impl<S> FrameCodec for WebCodecsCodec<S>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    fn decode_frame(
        &mut self,
        frame_data: &[u8],
        pts: Duration,
        _packet_desc: &[u8],
        out: &mut SampleBuffer,
    ) -> DecodeResult<u32> {
        if frame_data.is_empty() {
            if self.eof_flushed {
                out.clear();
                return Ok(0);
            }
            if !self.eof_draining {
                self.send(HostCmd::Flush {
                    decoder_id: self.decoder_id,
                    generation: self.generation,
                })?;
                self.eof_draining = true;
            }
            return self.drain_output(out);
        }

        self.eof_draining = false;
        self.eof_flushed = false;
        let pts_us = u64::try_from(pts.as_micros()).unwrap_or(u64::MAX);
        let mut data = self.pools.get_with_len::<u8>(frame_data.len())?;
        data.copy_from_slice(frame_data);
        self.send(HostCmd::Decode {
            pts_us,
            data,
            decoder_id: self.decoder_id,
            key: true,
            generation: self.generation,
        })?;
        self.poll_output(out)
    }

    fn decoded_pts(&self) -> Option<Duration> {
        Some(self.decoded_pts)
    }

    fn flush(&mut self) -> DecodeResult<()> {
        self.generation = self.generation.wrapping_add(1);
        self.send(HostCmd::Reset {
            decoder_id: self.decoder_id,
            generation: self.generation,
        })?;
        while self.out.try_recv().is_ok() {}
        self.eof_draining = false;
        self.eof_flushed = false;
        self.send_configure()?;
        tracing::debug!(
            decoder_id = self.decoder_id,
            codec = self.config.codec_string,
            generation = self.generation,
            "reset and reconfigured WebCodecs codec"
        );
        Ok(())
    }

    fn needs_eof_drain(&self, _source_sample_rate: u32) -> bool {
        true
    }

    fn spec(&self) -> AudioSpec {
        self.spec
    }

    fn track_info(&self) -> DecoderTrackInfo {
        self.track_info.clone()
    }
}

impl<S> Drop for WebCodecsCodec<S> {
    fn drop(&mut self) {
        self.cmd
            .send(HostCmd::Close {
                id: self.decoder_id,
            })
            .ok();
    }
}

impl<S> WebCodecsCodec<S> {
    fn send(&self, command: HostCmd) -> DecodeResult<()> {
        self.cmd
            .send(command)
            .map_err(|_| channel_disconnected("command"))
    }

    fn send_configure(&self) -> DecodeResult<()> {
        self.send(HostCmd::Configure {
            decoder_id: self.decoder_id,
            codec_string: self.config.codec_string.to_owned(),
            description: self.config.description.clone(),
            sample_rate: self.config.sample_rate,
            channels: self.config.channels,
            generation: self.generation,
        })
    }
}

#[must_use]
pub(crate) fn supports(codec: AudioCodec) -> bool {
    codec_string(codec).is_some() && super::probe::supported(codec)
}

fn codec_config(track: &TrackInfo) -> DecodeResult<CodecConfig> {
    let description = match track.codec {
        AudioCodec::AacLc | AudioCodec::AacHe | AudioCodec::AacHeV2 => {
            Some(Arc::from(track.extra_data.as_slice()))
        }
        AudioCodec::Mp3 => None,
        AudioCodec::Flac => Some(flac_description(&track.extra_data)?),
        codec => return Err(DecodeError::UnsupportedCodec { codec }),
    };
    Ok(CodecConfig {
        description,
        codec_string: codec_string(track.codec)
            .ok_or(DecodeError::UnsupportedCodec { codec: track.codec })?,
        sample_rate: track.sample_rate,
        channels: track.channels,
    })
}

pub(super) const fn codec_string(codec: AudioCodec) -> Option<&'static str> {
    match codec {
        AudioCodec::AacLc => Some("mp4a.40.2"),
        AudioCodec::AacHe => Some("mp4a.40.5"),
        AudioCodec::AacHeV2 => Some("mp4a.40.29"),
        AudioCodec::Mp3 => Some("mp3"),
        AudioCodec::Flac => Some("flac"),
        _ => None,
    }
}

fn flac_description(streaminfo: &[u8]) -> DecodeResult<Arc<[u8]>> {
    if streaminfo.len() != usize::from(Consts::FLAC_STREAMINFO_LEN) {
        return Err(DecodeError::InvalidData {
            detail: "WebCodecs FLAC description requires a 34-byte STREAMINFO payload",
        });
    }
    let mut description = [0; Consts::FLAC_DESCRIPTION_LEN];
    description[..4].copy_from_slice(b"fLaC");
    description[4..8].copy_from_slice(&[0x80, 0, 0, Consts::FLAC_STREAMINFO_LEN]);
    description[8..].copy_from_slice(streaminfo);
    Ok(Arc::from(description))
}

fn channel_disconnected(channel: &'static str) -> DecodeError {
    DecodeError::backend(WebCodecsError::ChannelDisconnected { channel })
}
