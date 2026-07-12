use kithara_bufpool::PcmBuf;
use kithara_platform::{
    sync::mpsc::{RecvTimeoutError, TryRecvError},
    time::{Duration, Instant},
};
use kithara_stream::AudioCodec;

use super::{
    host::{HostHandle, spawn_host},
    protocol::{HostCmd, HostOut},
};
use crate::{
    codec::FrameCodec,
    demuxer::TrackInfo,
    error::{DecodeError, DecodeResult},
    types::{DecoderTrackInfo, PcmSpec},
};

struct Consts;

impl Consts {
    const OUTPUT_TIMEOUT: Duration = Duration::from_millis(10);
    const FLAC_STREAMINFO_LEN: u8 = 34;
}

#[derive(Debug, thiserror::Error)]
enum WebCodecsError {
    #[error("WebCodecs backend failed: {detail}")]
    Backend { detail: String },
    #[error("WebCodecs host {channel} channel disconnected")]
    ChannelDisconnected { channel: &'static str },
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

pub(crate) struct WebCodecsCodec {
    host: HostHandle,
    generation: u64,
    spec: PcmSpec,
    track_info: DecoderTrackInfo,
    config: CodecConfig,
    eof_draining: bool,
}

impl WebCodecsCodec {
    pub(crate) fn open(track: &TrackInfo, gapless_enabled: bool) -> DecodeResult<Self> {
        let config = codec_config(track)?;
        let spec = PcmSpec::checked(
            track.channels,
            track.sample_rate,
            "webcodecs.track.sample_rate",
        )?;
        let host = spawn_host();
        send_configure(&host, &config, 0)?;
        tracing::debug!(
            codec = config.codec_string,
            generation = 0,
            "configured WebCodecs codec"
        );
        Ok(Self {
            host,
            generation: 0,
            spec,
            track_info: DecoderTrackInfo {
                gapless: if gapless_enabled { track.gapless } else { None },
                ..DecoderTrackInfo::default()
            },
            config,
            eof_draining: false,
        })
    }

    #[must_use]
    pub(crate) fn supports(codec: AudioCodec) -> bool {
        codec_string(codec).is_some() && super::probe::supported(codec)
    }

    fn poll_output(&mut self, out: &mut PcmBuf) -> DecodeResult<u32> {
        let first = match self
            .host
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
                None => match self.host.out.try_recv() {
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
                    let expected = usize::try_from(frames)
                        .ok()
                        .and_then(|frames| frames.checked_mul(usize::from(channels)));
                    if expected != Some(interleaved.len()) {
                        return Err(DecodeError::backend(WebCodecsError::OutputShape {
                            samples: interleaved.len(),
                            frames,
                            channels,
                        }));
                    }
                    out.ensure_len(interleaved.len())?;
                    out.copy_from_slice(&interleaved);
                    tracing::debug!(
                        generation,
                        pts_us,
                        sample_rate,
                        channels,
                        frames,
                        "received WebCodecs PCM"
                    );
                    return Ok(frames);
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
                HostOut::Pcm { generation, .. }
                | HostOut::Configured { generation, .. }
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
            if !self.eof_draining {
                self.send(HostCmd::Flush {
                    generation: self.generation,
                })?;
                self.eof_draining = true;
            }
            return self.poll_output(out);
        }

        self.eof_draining = false;
        let pts_us = u64::try_from(pts.as_micros()).unwrap_or(u64::MAX);
        self.send(HostCmd::Decode {
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
            generation: self.generation,
        })?;
        while self.host.out.try_recv().is_ok() {}
        self.eof_draining = false;
        send_configure(&self.host, &self.config, self.generation)?;
        tracing::debug!(
            codec = self.config.codec_string,
            generation = self.generation,
            "reset and reconfigured WebCodecs codec"
        );
        Ok(())
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
        let _ = self.host.cmd.send(HostCmd::Shutdown);
    }
}

impl WebCodecsCodec {
    fn send(&self, command: HostCmd) -> DecodeResult<()> {
        self.host
            .cmd
            .send(command)
            .map_err(|_| channel_disconnected("command"))
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

fn send_configure(host: &HostHandle, config: &CodecConfig, generation: u64) -> DecodeResult<()> {
    host.cmd
        .send(HostCmd::Configure {
            codec_string: config.codec_string.to_owned(),
            description: config.description.clone(),
            sample_rate: config.sample_rate,
            channels: config.channels,
            generation,
        })
        .map_err(|_| channel_disconnected("command"))
}

fn channel_disconnected(channel: &'static str) -> DecodeError {
    DecodeError::backend(WebCodecsError::ChannelDisconnected { channel })
}
