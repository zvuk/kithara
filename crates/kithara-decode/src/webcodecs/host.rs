use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    rc::Rc,
};

use js_sys::{Float32Array, Object, Uint8Array};
use kithara_platform::{
    sync::mpsc,
    thread::{assert_not_main_thread, keep_worker_alive, spawn_named},
    tokio::task::spawn as task_spawn,
};
use num_traits::ToPrimitive;
use wasm_bindgen::{JsCast, JsValue, closure::Closure};
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    AudioData, AudioDataCopyToOptions, AudioDecoder, AudioDecoderConfig, AudioDecoderInit,
    AudioSampleFormat, EncodedAudioChunk, EncodedAudioChunkInit, EncodedAudioChunkType,
};

use super::protocol::{HostCmd, HostOut};
use crate::{DecodeError, DecodeResult};

pub(crate) struct HostHandle {
    pub(crate) cmd: mpsc::Sender<HostCmd>,
    pub(crate) out: mpsc::Receiver<HostOut>,
}

struct DecoderHost {
    decoder: AudioDecoder,
    pending: Rc<RefCell<VecDeque<(u64, u64)>>>,
    _error_callback: Closure<dyn FnMut(JsValue)>,
    _output_callback: Closure<dyn FnMut(AudioData)>,
}

#[derive(Debug, thiserror::Error)]
enum HostError {
    #[error("WebCodecs {op} failed: {detail}")]
    Api { op: &'static str, detail: String },
    #[error("WebCodecs AudioData has no sample format")]
    MissingFormat,
    #[error("WebCodecs AudioData channel count {channels} exceeds u16")]
    ChannelCount { channels: u32 },
    #[error("WebCodecs {field} value is invalid: {value}")]
    Number { field: &'static str, value: f64 },
    #[error("WebCodecs PCM buffer size overflow: {frames} frames, {channels} channels")]
    BufferSize { frames: u32, channels: u32 },
}

pub(crate) fn spawn_host() -> HostHandle {
    let (cmd, cmd_rx) = mpsc::channel();
    let (out_tx, out) = mpsc::channel();
    let worker = spawn_named("kithara-webcodecs-host", move || host_main(cmd_rx, out_tx));
    std::mem::forget(worker);
    HostHandle { cmd, out }
}

fn host_main(cmd_rx: mpsc::Receiver<HostCmd>, out_tx: mpsc::Sender<HostOut>) {
    assert_not_main_thread(concat!(module_path!(), "::host_main"));
    keep_worker_alive();

    task_spawn(async move {
        let generation = Rc::new(Cell::new(0));
        let announced_generation = Rc::new(Cell::new(None));
        let mut host = None;

        while let Ok(cmd) = cmd_rx.recv_async().await {
            if dispatch(cmd, &mut host, &out_tx, &generation, &announced_generation).await {
                break;
            }
        }

        if let Some(host) = host
            && let Err(err) = host.decoder.close()
        {
            tracing::warn!(detail = %js_detail(&err), "failed to close WebCodecs decoder");
        }
    });
}

async fn dispatch(
    cmd: HostCmd,
    host: &mut Option<DecoderHost>,
    out_tx: &mpsc::Sender<HostOut>,
    current_generation: &Rc<Cell<u64>>,
    announced_generation: &Rc<Cell<Option<u64>>>,
) -> bool {
    match cmd {
        HostCmd::Configure {
            codec_string,
            description,
            sample_rate,
            channels,
            generation,
        } => {
            if generation < current_generation.get() {
                return false;
            }
            current_generation.set(generation);
            announced_generation.set(None);
            if host.is_none() {
                *host = match DecoderHost::new(
                    out_tx.clone(),
                    Rc::clone(current_generation),
                    Rc::clone(announced_generation),
                ) {
                    Ok(host) => Some(host),
                    Err(err) => {
                        send_error(out_tx, &err, generation);
                        None
                    }
                };
            }
            if let Some(host) = host
                && let Err(err) =
                    host.configure(&codec_string, description.as_deref(), sample_rate, channels)
            {
                tracing::error!(codec = %codec_string, generation, error = %err, "failed to configure WebCodecs decoder");
                send_error(out_tx, &err, generation);
            }
        }
        HostCmd::Decode {
            data,
            pts_us,
            key,
            generation,
        } => {
            if generation != current_generation.get() {
                return false;
            }
            let result = host
                .as_ref()
                .ok_or_else(|| {
                    DecodeError::backend(HostError::Api {
                        op: "decode",
                        detail: "decoder is not configured".to_owned(),
                    })
                })
                .and_then(|host| host.decode(&data, pts_us, key, generation));
            if let Err(err) = result {
                tracing::error!(generation, pts_us, error = %err, "failed to queue WebCodecs chunk");
                send_error(out_tx, &err, generation);
            }
        }
        HostCmd::Reset { generation } => {
            if generation < current_generation.get() {
                return false;
            }
            current_generation.set(generation);
            announced_generation.set(None);
            if let Some(host) = host {
                host.pending.borrow_mut().clear();
                if let Err(err) = host.decoder.reset() {
                    let err = api_error("reset", &err);
                    tracing::error!(generation, error = %err, "failed to reset WebCodecs decoder");
                    send_error(out_tx, &err, generation);
                }
            }
        }
        HostCmd::Flush { generation } => {
            if generation != current_generation.get() {
                return false;
            }
            let result = match host {
                Some(host) => JsFuture::from(host.decoder.flush())
                    .await
                    .map(|_| ())
                    .map_err(|err| api_error("flush", &err)),
                None => Err(DecodeError::backend(HostError::Api {
                    op: "flush",
                    detail: "decoder is not configured".to_owned(),
                })),
            };
            if let Err(err) = result {
                tracing::error!(generation, error = %err, "failed to flush WebCodecs decoder");
                send_error(out_tx, &err, generation);
            }
        }
        HostCmd::Shutdown => return true,
    }
    false
}

impl DecoderHost {
    fn new(
        out_tx: mpsc::Sender<HostOut>,
        generation: Rc<Cell<u64>>,
        announced_generation: Rc<Cell<Option<u64>>>,
    ) -> DecodeResult<Self> {
        let error_tx = out_tx.clone();
        let error_generation = Rc::clone(&generation);
        let error_callback = Closure::new(move |value: JsValue| {
            let generation = error_generation.get();
            let detail = js_detail(&value);
            tracing::error!(generation, detail = %detail, "WebCodecs decoder callback failed");
            let _ = error_tx.send(HostOut::Error { detail, generation });
        });

        let pending = Rc::new(RefCell::new(VecDeque::new()));
        let output_pending = Rc::clone(&pending);
        let output_callback = Closure::new(move |data: AudioData| {
            let pts_us = data.timestamp().to_u64();
            let Some((input_pts_us, output_generation)) = output_pending.borrow_mut().pop_front()
            else {
                data.close();
                tracing::debug!(pts_us, "dropping unmatched WebCodecs output");
                return;
            };
            if pts_us != Some(input_pts_us) {
                tracing::debug!(input_pts_us, output_pts_us = ?pts_us, "WebCodecs adjusted output timestamp");
            }
            let generation = generation.get();
            if output_generation != generation {
                data.close();
                tracing::debug!(
                    output_generation,
                    generation,
                    "dropping stale WebCodecs output"
                );
                return;
            }
            match copy_audio(&data, output_generation) {
                Ok(output) => {
                    if announced_generation.get() != Some(generation)
                        && let HostOut::Pcm {
                            sample_rate,
                            channels,
                            ..
                        } = &output
                    {
                        let _ = out_tx.send(HostOut::Configured {
                            sample_rate: *sample_rate,
                            channels: *channels,
                            generation,
                        });
                        announced_generation.set(Some(generation));
                    }
                    let _ = out_tx.send(output);
                }
                Err(err) => {
                    tracing::error!(generation, error = %err, "failed to copy WebCodecs AudioData");
                    send_error(&out_tx, &err, generation);
                }
            }
        });

        let init = AudioDecoderInit::new(
            error_callback.as_ref().unchecked_ref(),
            output_callback.as_ref().unchecked_ref(),
        );
        let decoder = AudioDecoder::new(&init).map_err(|err| api_error("create", &err))?;
        Ok(Self {
            decoder,
            pending,
            _error_callback: error_callback,
            _output_callback: output_callback,
        })
    }

    fn configure(
        &self,
        codec: &str,
        description: Option<&[u8]>,
        sample_rate: u32,
        channels: u16,
    ) -> DecodeResult<()> {
        self.pending.borrow_mut().clear();
        let config: AudioDecoderConfig = Object::new().unchecked_into();
        config.set_codec(codec);
        config.set_sample_rate(sample_rate);
        config.set_number_of_channels(u32::from(channels));
        if let Some(description) = description {
            let description = Uint8Array::from(description);
            config.set_description_u8_array(&description);
        }
        self.decoder
            .configure(&config)
            .map_err(|err| api_error("configure", &err))
    }

    fn decode(&self, data: &[u8], pts_us: u64, key: bool, generation: u64) -> DecodeResult<()> {
        let data = Uint8Array::from(data);
        let chunk_type = if key {
            EncodedAudioChunkType::Key
        } else {
            EncodedAudioChunkType::Delta
        };
        let init = EncodedAudioChunkInit::new_with_u8_array(&data, 0, chunk_type);
        let timestamp = pts_us.to_f64().ok_or_else(|| {
            DecodeError::backend(HostError::Number {
                field: "timestamp",
                value: f64::INFINITY,
            })
        })?;
        init.set_timestamp_f64(timestamp);
        let chunk = EncodedAudioChunk::new(&init).map_err(|err| api_error("chunk", &err))?;
        self.decoder
            .decode(&chunk)
            .map_err(|err| api_error("decode", &err))?;
        self.pending.borrow_mut().push_back((pts_us, generation));
        Ok(())
    }
}

fn copy_audio(data: &AudioData, generation: u64) -> DecodeResult<HostOut> {
    let result = copy_audio_inner(data, generation);
    data.close();
    result
}

fn copy_audio_inner(data: &AudioData, generation: u64) -> DecodeResult<HostOut> {
    let frames = data.number_of_frames();
    let channel_count = data.number_of_channels();
    let channels = u16::try_from(channel_count).map_err(|_| {
        DecodeError::backend(HostError::ChannelCount {
            channels: channel_count,
        })
    })?;
    let sample_rate_value = f64::from(data.sample_rate());
    let sample_rate = sample_rate_value.to_u32().ok_or_else(|| {
        DecodeError::backend(HostError::Number {
            field: "sample rate",
            value: sample_rate_value,
        })
    })?;
    let pts_value = data.timestamp();
    let pts_us = pts_value.to_u64().ok_or_else(|| {
        DecodeError::backend(HostError::Number {
            field: "timestamp",
            value: pts_value,
        })
    })?;
    let sample_count = usize::try_from(frames)
        .ok()
        .and_then(|frames| frames.checked_mul(usize::from(channels)))
        .ok_or_else(|| {
            DecodeError::backend(HostError::BufferSize {
                frames,
                channels: channel_count,
            })
        })?;
    let format = data
        .format()
        .ok_or_else(|| DecodeError::backend(HostError::MissingFormat))?;
    let interleaved = if matches!(
        format,
        AudioSampleFormat::U8Planar
            | AudioSampleFormat::S16Planar
            | AudioSampleFormat::S32Planar
            | AudioSampleFormat::F32Planar
    ) {
        copy_planar(data, frames, channels, sample_count)?
    } else {
        copy_interleaved(data, sample_count)?
    };

    Ok(HostOut::Pcm {
        interleaved,
        frames,
        sample_rate,
        channels,
        pts_us,
        generation,
    })
}

fn copy_interleaved(data: &AudioData, sample_count: usize) -> DecodeResult<Vec<f32>> {
    let length = u32::try_from(sample_count).map_err(|_| {
        DecodeError::backend(HostError::BufferSize {
            frames: data.number_of_frames(),
            channels: data.number_of_channels(),
        })
    })?;
    let buffer = Float32Array::new_with_length(length);
    let options = AudioDataCopyToOptions::new(0);
    options.set_format(AudioSampleFormat::F32);
    data.copy_to_with_buffer_source(buffer.as_ref(), &options)
        .map_err(|err| api_error("copyTo", &err))?;
    Ok(buffer.to_vec())
}

fn copy_planar(
    data: &AudioData,
    frames: u32,
    channels: u16,
    sample_count: usize,
) -> DecodeResult<Vec<f32>> {
    let mut interleaved = vec![0.0; sample_count];
    for channel in 0..channels {
        let buffer = Float32Array::new_with_length(frames);
        let options = AudioDataCopyToOptions::new(u32::from(channel));
        options.set_format(AudioSampleFormat::F32Planar);
        data.copy_to_with_buffer_source(buffer.as_ref(), &options)
            .map_err(|err| api_error("copyTo", &err))?;
        for (frame, sample) in buffer.to_vec().into_iter().enumerate() {
            interleaved[frame * usize::from(channels) + usize::from(channel)] = sample;
        }
    }
    Ok(interleaved)
}

fn api_error(op: &'static str, value: &JsValue) -> DecodeError {
    DecodeError::backend(HostError::Api {
        op,
        detail: js_detail(value),
    })
}

fn js_detail(value: &JsValue) -> String {
    value.as_string().unwrap_or_else(|| format!("{value:?}"))
}

fn send_error(out_tx: &mpsc::Sender<HostOut>, err: &DecodeError, generation: u64) {
    let _ = out_tx.send(HostOut::Error {
        detail: err.to_string(),
        generation,
    });
}
