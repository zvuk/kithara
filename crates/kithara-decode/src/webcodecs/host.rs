use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, VecDeque},
    rc::Rc,
};

use js_sys::{Float32Array, Object, Uint8Array};
use kithara_platform::{
    sync::mpsc::{self, TryRecvError},
    thread::{assert_not_main_thread, keep_worker_alive, spawn_named},
    time::{self, Duration},
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

struct DecoderHost {
    decoder: AudioDecoder,
    pending: Rc<RefCell<VecDeque<(u64, u64)>>>,
    _error_callback: Closure<dyn FnMut(JsValue)>,
    _output_callback: Closure<dyn FnMut(AudioData)>,
}

struct DecoderState {
    host: DecoderHost,
    out_tx: mpsc::Sender<HostOut>,
    generation: Rc<Cell<u64>>,
    announced_generation: Rc<Cell<Option<u64>>>,
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

pub(crate) fn spawn_host() -> mpsc::Sender<HostCmd> {
    let (cmd, cmd_rx) = mpsc::channel();
    let worker = spawn_named("kithara-webcodecs-host", move || host_main(cmd_rx));
    std::mem::forget(worker);
    cmd
}

fn host_main(cmd_rx: mpsc::Receiver<HostCmd>) {
    assert_not_main_thread(concat!(module_path!(), "::host_main"));
    keep_worker_alive();

    task_spawn(async move {
        const POLL_IDLE: Duration = Duration::from_millis(4);
        let mut decoders = HashMap::new();

        'outer: loop {
            let mut worked = false;
            loop {
                match cmd_rx.try_recv() {
                    Ok(cmd) => {
                        worked = true;
                        dispatch(cmd, &mut decoders).await;
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(_) => break 'outer,
                }
            }
            let pause = if worked { Duration::ZERO } else { POLL_IDLE };
            time::sleep(pause).await;
        }

        for (decoder_id, state) in decoders {
            close_decoder(decoder_id, state);
        }
    });
}

async fn dispatch(cmd: HostCmd, decoders: &mut HashMap<u64, DecoderState>) {
    match cmd {
        HostCmd::Open { id, reply_tx } => on_open(id, reply_tx, decoders),
        HostCmd::Configure {
            decoder_id,
            codec_string,
            description,
            sample_rate,
            channels,
            generation,
        } => {
            let Some(state) = decoders.get_mut(&decoder_id) else {
                tracing::warn!(
                    decoder_id,
                    generation,
                    "ignoring configure for unknown WebCodecs decoder"
                );
                return;
            };
            if generation < state.generation.get() {
                return;
            }
            state.generation.set(generation);
            state.announced_generation.set(None);
            if let Err(err) =
                state
                    .host
                    .configure(&codec_string, description.as_deref(), sample_rate, channels)
            {
                tracing::error!(decoder_id, codec = %codec_string, generation, error = %err, "failed to configure WebCodecs decoder");
                send_error(&state.out_tx, &err, generation);
            }
        }
        HostCmd::Decode {
            decoder_id,
            data,
            pts_us,
            key,
            generation,
        } => {
            let Some(state) = decoders.get(&decoder_id) else {
                tracing::warn!(
                    decoder_id,
                    generation,
                    "ignoring decode for unknown WebCodecs decoder"
                );
                return;
            };
            if generation != state.generation.get() {
                return;
            }
            let result = state.host.decode(&data, pts_us, key, generation);
            if let Err(err) = result {
                tracing::error!(decoder_id, generation, pts_us, error = %err, "failed to queue WebCodecs chunk");
                send_error(&state.out_tx, &err, generation);
            }
        }
        HostCmd::Reset {
            decoder_id,
            generation,
        } => {
            let Some(state) = decoders.get_mut(&decoder_id) else {
                tracing::warn!(
                    decoder_id,
                    generation,
                    "ignoring reset for unknown WebCodecs decoder"
                );
                return;
            };
            if generation < state.generation.get() {
                return;
            }
            state.generation.set(generation);
            state.announced_generation.set(None);
            state.host.pending.borrow_mut().clear();
            if let Err(err) = state.host.decoder.reset() {
                let err = api_error("reset", &err);
                tracing::error!(decoder_id, generation, error = %err, "failed to reset WebCodecs decoder");
                send_error(&state.out_tx, &err, generation);
            }
        }
        HostCmd::Flush {
            decoder_id,
            generation,
        } => {
            let Some(state) = decoders.get(&decoder_id) else {
                tracing::warn!(
                    decoder_id,
                    generation,
                    "ignoring flush for unknown WebCodecs decoder"
                );
                return;
            };
            if generation != state.generation.get() {
                return;
            }
            let result = JsFuture::from(state.host.decoder.flush())
                .await
                .map(|_| ())
                .map_err(|err| api_error("flush", &err));
            match result {
                Ok(()) => {
                    let _ = state.out_tx.send(HostOut::Flushed { generation });
                }
                Err(err) => {
                    tracing::error!(decoder_id, generation, error = %err, "failed to flush WebCodecs decoder");
                    send_error(&state.out_tx, &err, generation);
                }
            }
        }
        HostCmd::Close { id } => {
            if let Some(state) = decoders.remove(&id) {
                close_decoder(id, state);
            }
        }
    }
}

fn on_open(
    decoder_id: u64,
    out_tx: mpsc::Sender<HostOut>,
    decoders: &mut HashMap<u64, DecoderState>,
) {
    if decoders.contains_key(&decoder_id) {
        let err = DecodeError::backend(HostError::Api {
            op: "open",
            detail: "decoder id is already registered".to_owned(),
        });
        send_error(&out_tx, &err, 0);
        return;
    }
    let generation = Rc::new(Cell::new(0));
    let announced_generation = Rc::new(Cell::new(None));
    match DecoderHost::new(
        decoder_id,
        out_tx.clone(),
        Rc::clone(&generation),
        Rc::clone(&announced_generation),
    ) {
        Ok(host) => {
            decoders.insert(
                decoder_id,
                DecoderState {
                    host,
                    out_tx,
                    generation,
                    announced_generation,
                },
            );
            tracing::debug!(decoder_id, "opened WebCodecs decoder");
        }
        Err(err) => {
            tracing::error!(decoder_id, error = %err, "failed to open WebCodecs decoder");
            send_error(&out_tx, &err, 0);
        }
    }
}

fn close_decoder(decoder_id: u64, state: DecoderState) {
    let DecoderState { host, .. } = state;
    if let Err(err) = host.decoder.close() {
        tracing::warn!(decoder_id, detail = %js_detail(&err), "failed to close WebCodecs decoder");
    } else {
        tracing::debug!(decoder_id, "closed WebCodecs decoder");
    }
}

impl DecoderHost {
    fn new(
        decoder_id: u64,
        out_tx: mpsc::Sender<HostOut>,
        generation: Rc<Cell<u64>>,
        announced_generation: Rc<Cell<Option<u64>>>,
    ) -> DecodeResult<Self> {
        let error_tx = out_tx.clone();
        let error_generation = Rc::clone(&generation);
        let error_callback = Closure::new(move |value: JsValue| {
            let generation = error_generation.get();
            let detail = js_detail(&value);
            tracing::error!(decoder_id, generation, detail = %detail, "WebCodecs decoder callback failed");
            let _ = error_tx.send(HostOut::Error { detail, generation });
        });

        let pending = Rc::new(RefCell::new(VecDeque::new()));
        let output_pending = Rc::clone(&pending);
        let output_callback = Closure::new(move |data: AudioData| {
            let pts_us = data.timestamp().to_u64();
            let Some((input_pts_us, output_generation)) = output_pending.borrow_mut().pop_front()
            else {
                data.close();
                tracing::debug!(decoder_id, pts_us, "dropping unmatched WebCodecs output");
                return;
            };
            if pts_us != Some(input_pts_us) {
                tracing::debug!(decoder_id, input_pts_us, output_pts_us = ?pts_us, "WebCodecs adjusted output timestamp");
            }
            let generation = generation.get();
            if output_generation != generation {
                data.close();
                tracing::debug!(
                    output_generation,
                    generation,
                    decoder_id,
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
                    tracing::error!(decoder_id, generation, error = %err, "failed to copy WebCodecs AudioData");
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

#[cfg(test)]
mod tests {
    use kithara_platform::time::{Duration, Instant};
    use kithara_test_utils::kithara;

    use super::*;
    use crate::webcodecs::protocol::{HostCmd, HostOut};

    fn open_host() -> (mpsc::Sender<HostCmd>, mpsc::Receiver<HostOut>) {
        let cmd = spawn_host();
        let (reply_tx, out) = mpsc::channel();
        cmd.send(HostCmd::Open { id: 1, reply_tx })
            .expect("BUG: host cmd channel closed at spawn");
        (cmd, out)
    }

    #[kithara::test(wasm, timeout(Duration::from_secs(120)))]
    async fn composed_mp3_reaches_eof_with_pcm() {
        use symphonia::{
            core::{
                formats::{FormatOptions, probe::Hint},
                io::{MediaSourceStream, MediaSourceStreamOptions},
                meta::MetadataOptions,
            },
            default,
        };

        use crate::{
            composed::{ComposedDecoder, DecoderRuntime},
            demuxer::Demuxer,
            traits::{Decoder, DecoderChunkOutcome},
            webcodecs::codec::WebCodecsCodec,
        };

        crate::webcodecs::probe::spawn_webcodecs_probe();

        const TEST_MP3_BYTES: &[u8] = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../assets/test.mp3"
        ));
        let cursor = std::io::Cursor::new(TEST_MP3_BYTES.to_vec());
        let mss = MediaSourceStream::new(Box::new(cursor), MediaSourceStreamOptions::default());
        let mut hint = Hint::new();
        hint.with_extension("mp3");
        let format_reader = default::get_probe()
            .probe(
                &hint,
                mss,
                FormatOptions::default(),
                MetadataOptions::default(),
            )
            .expect("BUG: MP3 probe should succeed");
        let demuxer =
            crate::symphonia::SymphoniaDemuxer::from_reader_with_layout(format_reader, None, None)
                .expect("BUG: MP3 demuxer should build");
        let track = demuxer.track_info().clone();
        let codec = WebCodecsCodec::open(&track, false).expect("BUG: WebCodecs open");
        let mut decoder = ComposedDecoder::new(demuxer, codec, DecoderRuntime::for_test());

        let mut total_frames: u64 = 0;
        let mut outcomes: u32 = 0;
        loop {
            outcomes += 1;
            assert!(outcomes < 100_000, "runaway next_chunk loop");
            match decoder.next_chunk().expect("next_chunk") {
                DecoderChunkOutcome::Chunk(chunk) => {
                    total_frames += chunk.frames() as u64;
                }
                DecoderChunkOutcome::Pending(_) => {
                    kithara_platform::time::sleep(Duration::from_millis(1)).await;
                }
                DecoderChunkOutcome::Eof => break,
            }
        }
        assert!(total_frames > 0, "no PCM through ComposedDecoder");
    }

    #[kithara::test(wasm, timeout(Duration::from_secs(60)))]
    async fn real_mp3_frames_produce_pcm() {
        use kithara_bufpool::PcmPool;
        use symphonia::{
            core::{
                formats::{FormatOptions, probe::Hint},
                io::{MediaSourceStream, MediaSourceStreamOptions},
                meta::MetadataOptions,
            },
            default,
        };

        use crate::{
            codec::FrameCodec,
            demuxer::{DemuxOutcome, Demuxer},
            symphonia::SymphoniaDemuxer,
            webcodecs::codec::WebCodecsCodec,
        };

        crate::webcodecs::probe::spawn_webcodecs_probe();

        const TEST_MP3_BYTES: &[u8] = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../assets/test.mp3"
        ));
        let cursor = std::io::Cursor::new(TEST_MP3_BYTES.to_vec());
        let mss = MediaSourceStream::new(Box::new(cursor), MediaSourceStreamOptions::default());
        let mut hint = Hint::new();
        hint.with_extension("mp3");
        let format_reader = default::get_probe()
            .probe(
                &hint,
                mss,
                FormatOptions::default(),
                MetadataOptions::default(),
            )
            .expect("BUG: MP3 probe should succeed");
        let mut demuxer = SymphoniaDemuxer::from_reader_with_layout(format_reader, None, None)
            .expect("BUG: MP3 demuxer should build");
        let track = demuxer.track_info().clone();
        let mut codec = WebCodecsCodec::open(&track, false).expect("BUG: WebCodecs open");
        let pool = PcmPool::default();
        let mut buf = pool.get();

        for step in 0..64_u32 {
            let (data, pts) = match demuxer.next_frame().expect("demux") {
                DemuxOutcome::Frame(frame) => (frame.data.to_vec(), frame.pts),
                other => panic!("unexpected demux outcome: {other:?}"),
            };
            let frames = codec
                .decode_frame(&data, pts, &[], &mut buf)
                .expect("decode_frame");
            tracing::warn!(step, frames, bytes = data.len(), "mp3 e2e probe step");
            if frames > 0 {
                return;
            }
            kithara_platform::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("no PCM from WebCodecs for 64 real mp3 frames");
    }

    #[kithara::test(wasm, timeout(Duration::from_secs(30)))]
    async fn host_flushes_empty_configured_decoder() {
        let (cmd, out) = open_host();
        cmd.send(HostCmd::Configure {
            decoder_id: 1,
            codec_string: "mp3".to_owned(),
            description: None,
            sample_rate: 44_100,
            channels: 2,
            generation: 0,
        })
        .expect("BUG: host cmd channel closed at spawn");
        cmd.send(HostCmd::Flush {
            decoder_id: 1,
            generation: 0,
        })
        .expect("BUG: host cmd channel closed after configure");
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Ok(output) = out.try_recv() {
                assert!(
                    matches!(output, HostOut::Flushed { generation: 0 }),
                    "expected HostOut::Flushed, got {output:?}"
                );
                return;
            }
            assert!(
                Instant::now() <= deadline,
                "flush promise never resolved on an empty configured decoder"
            );
            kithara_platform::time::sleep(Duration::from_millis(100)).await;
        }
    }

    #[kithara::test(wasm, timeout(Duration::from_secs(30)))]
    async fn host_replies_error_to_garbage_configure() {
        let (cmd, out) = open_host();
        cmd.send(HostCmd::Configure {
            decoder_id: 1,
            codec_string: "garbage/codec".to_owned(),
            description: None,
            sample_rate: 44_100,
            channels: 2,
            generation: 0,
        })
        .expect("BUG: host cmd channel closed at spawn");
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Ok(output) = out.try_recv() {
                assert!(
                    matches!(output, HostOut::Error { .. }),
                    "expected HostOut::Error, got {output:?}"
                );
                return;
            }
            assert!(
                Instant::now() <= deadline,
                "host task never replied to invalid configure"
            );
            kithara_platform::time::sleep(Duration::from_millis(100)).await;
        }
    }
}
