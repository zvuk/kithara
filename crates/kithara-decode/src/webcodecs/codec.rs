use std::sync::atomic::{AtomicU64, Ordering};

use kithara_bufpool::PcmBuf;
use kithara_platform::{
    sync::mpsc::{self, RecvTimeoutError, TryRecvError},
    time::{Duration, Instant},
};
use kithara_stream::AudioCodec;

use super::protocol::{HostCmd, HostOut};
use crate::{
    codec::FrameCodec,
    demuxer::TrackInfo,
    error::{DecodeError, DecodeResult},
    types::{DecoderTrackInfo, PcmSpec},
};

struct Consts;

static NEXT_DECODER_ID: AtomicU64 = AtomicU64::new(1);

impl Consts {
    const DRAIN_TIMEOUT: Duration = Duration::from_millis(250);
    const OUTPUT_TIMEOUT: Duration = Duration::from_millis(10);
    const FLAC_STREAMINFO_LEN: u8 = 34;
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
    description: Option<Vec<u8>>,
    sample_rate: u32,
    channels: u16,
}

struct PcmOut<'a> {
    interleaved: &'a [f32],
    frames: u32,
    sample_rate: u32,
    channels: u16,
    pts_us: u64,
    generation: u64,
}

pub(crate) struct WebCodecsCodec {
    decoder_id: u64,
    cmd: mpsc::Sender<HostCmd>,
    out: mpsc::Receiver<HostOut>,
    generation: u64,
    spec: PcmSpec,
    track_info: DecoderTrackInfo,
    config: CodecConfig,
    eof_draining: bool,
    eof_flushed: bool,
}

impl WebCodecsCodec {
    pub(crate) fn open(track: &TrackInfo, gapless_enabled: bool) -> DecodeResult<Self> {
        let config = codec_config(track)?;
        let spec = PcmSpec::checked(
            track.channels,
            track.sample_rate,
            "webcodecs.track.sample_rate",
        )?;
        let cmd = super::probe::host_sender()
            .ok_or_else(|| DecodeError::backend(WebCodecsError::RuntimeNotInitialized))?;
        let decoder_id = NEXT_DECODER_ID.fetch_add(1, Ordering::Relaxed);
        let (reply_tx, out) = mpsc::channel();
        cmd.send(HostCmd::Open {
            id: decoder_id,
            reply_tx,
        })
        .map_err(|_| channel_disconnected("command"))?;
        let codec = Self {
            decoder_id,
            cmd,
            out,
            generation: 0,
            spec,
            track_info: DecoderTrackInfo {
                gapless: if gapless_enabled { track.gapless } else { None },
                ..DecoderTrackInfo::default()
            },
            config,
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

    #[must_use]
    pub(crate) fn supports(codec: AudioCodec) -> bool {
        codec_string(codec).is_some() && super::probe::supported(codec)
    }

    fn poll_output(&mut self, out: &mut PcmBuf) -> DecodeResult<u32> {
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
                        interleaved: &interleaved,
                        frames,
                        sample_rate,
                        channels,
                        pts_us,
                        generation,
                    };
                    return self.write_pcm(out, &pcm);
                }
                HostOut::Configured {
                    sample_rate,
                    channels,
                    generation,
                } if generation == self.generation => {
                    self.spec =
                        PcmSpec::checked(channels, sample_rate, "webcodecs.output.sample_rate")?;
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

    fn drain_output(&mut self, out: &mut PcmBuf) -> DecodeResult<u32> {
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
                        interleaved: &interleaved,
                        frames,
                        sample_rate,
                        channels,
                        pts_us,
                        generation,
                    };
                    return self.write_pcm(out, &pcm);
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
                        PcmSpec::checked(channels, sample_rate, "webcodecs.output.sample_rate")?;
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

    fn write_pcm(&self, out: &mut PcmBuf, pcm: &PcmOut<'_>) -> DecodeResult<u32> {
        let expected = usize::try_from(pcm.frames)
            .ok()
            .and_then(|frames| frames.checked_mul(usize::from(pcm.channels)));
        if expected != Some(pcm.interleaved.len()) {
            return Err(DecodeError::backend(WebCodecsError::OutputShape {
                samples: pcm.interleaved.len(),
                frames: pcm.frames,
                channels: pcm.channels,
            }));
        }
        out.ensure_len(pcm.interleaved.len())?;
        out.copy_from_slice(pcm.interleaved);
        tracing::debug!(
            generation = pcm.generation,
            decoder_id = self.decoder_id,
            pts_us = pcm.pts_us,
            sample_rate = pcm.sample_rate,
            channels = pcm.channels,
            frames = pcm.frames,
            "received WebCodecs PCM"
        );
        Ok(pcm.frames)
    }
}

impl FrameCodec for WebCodecsCodec {
    fn decode_frame(
        &mut self,
        frame_data: &[u8],
        pts: Duration,
        _packet_desc: &[u8],
        out: &mut PcmBuf,
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
        self.send(HostCmd::Decode {
            decoder_id: self.decoder_id,
            data: frame_data.to_vec(),
            pts_us,
            key: true,
            generation: self.generation,
        })?;
        self.poll_output(out)
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

    fn spec(&self) -> PcmSpec {
        self.spec
    }

    fn track_info(&self) -> DecoderTrackInfo {
        self.track_info.clone()
    }
}

impl Drop for WebCodecsCodec {
    fn drop(&mut self) {
        let _ = self.cmd.send(HostCmd::Close {
            id: self.decoder_id,
        });
    }
}

impl WebCodecsCodec {
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

fn codec_config(track: &TrackInfo) -> DecodeResult<CodecConfig> {
    let description = match track.codec {
        AudioCodec::AacLc | AudioCodec::AacHe | AudioCodec::AacHeV2 => {
            Some(track.extra_data.clone())
        }
        AudioCodec::Mp3 => None,
        AudioCodec::Flac => Some(flac_description(&track.extra_data)?),
        codec => return Err(DecodeError::UnsupportedCodec { codec }),
    };
    Ok(CodecConfig {
        codec_string: codec_string(track.codec)
            .ok_or(DecodeError::UnsupportedCodec { codec: track.codec })?,
        description,
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

fn flac_description(streaminfo: &[u8]) -> DecodeResult<Vec<u8>> {
    if streaminfo.len() != usize::from(Consts::FLAC_STREAMINFO_LEN) {
        return Err(DecodeError::InvalidData {
            detail: "WebCodecs FLAC description requires a 34-byte STREAMINFO payload",
        });
    }
    let mut description = Vec::with_capacity(4 + 4 + streaminfo.len());
    description.extend_from_slice(b"fLaC");
    description.extend_from_slice(&[0x80, 0, 0, Consts::FLAC_STREAMINFO_LEN]);
    description.extend_from_slice(streaminfo);
    Ok(description)
}

fn channel_disconnected(channel: &'static str) -> DecodeError {
    DecodeError::backend(WebCodecsError::ChannelDisconnected { channel })
}
