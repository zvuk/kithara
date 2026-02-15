use std::io::Cursor;

use symphonia::core::{
    codecs::{CodecParameters, audio::AudioDecoderOptions},
    errors::Error as SymphoniaError,
    formats::{FormatOptions, TrackType, probe::Hint},
    io::{MediaSourceStream, MediaSourceStreamOptions},
    meta::MetadataOptions,
};
use tracing::info;

/// Decode audio file bytes and log PCM info.
#[expect(
    clippy::cognitive_complexity,
    reason = "demo function with linear flow"
)]
fn decode_file(data: Vec<u8>, filename: &str) {
    let data_len = data.len();
    info!(filename, bytes = data_len, "Starting decode");

    // Build probe hint from filename extension.
    let mut hint = Hint::new();
    if let Some(ext) = filename.rsplit('.').next() {
        hint.with_extension(ext);
    }

    // Wrap bytes in Cursor → MediaSourceStream.
    let cursor = Cursor::new(data);
    let mss = MediaSourceStream::new(Box::new(cursor), MediaSourceStreamOptions::default());

    let format_opts = FormatOptions::default();
    let meta_opts = MetadataOptions::default();

    // Probe format.
    let mut format_reader =
        match symphonia::default::get_probe().probe(&hint, mss, format_opts, meta_opts) {
            Ok(reader) => reader,
            Err(e) => {
                tracing::error!(error = %e, "Probe failed");
                return;
            }
        };

    // Find the audio track.
    let track = if let Some(t) = format_reader.default_track(TrackType::Audio) {
        t.clone()
    } else {
        tracing::error!("No audio track found");
        return;
    };

    let track_id = track.id;

    // Extract codec parameters.
    let codec_params = if let Some(CodecParameters::Audio(params)) = &track.codec_params {
        params.clone()
    } else {
        tracing::error!("No audio codec parameters");
        return;
    };

    let sample_rate = codec_params.sample_rate.unwrap_or(0);
    let channels = codec_params.channels.as_ref().map_or(0, |c| {
        #[expect(clippy::cast_possible_truncation)] // channel count fits u16
        let ch = c.count() as u16;
        ch
    });
    let codec_name = format!("{:?}", codec_params.codec);

    info!(codec = %codec_name, sample_rate, channels, "Audio track found");

    // Create decoder.
    let decoder_opts = AudioDecoderOptions { verify: false };
    let mut decoder =
        match symphonia::default::get_codecs().make_audio_decoder(&codec_params, &decoder_opts) {
            Ok(d) => d,
            Err(e) => {
                tracing::error!(error = %e, "Failed to create decoder");
                return;
            }
        };

    // Decode all packets.
    let mut total_frames: u64 = 0;
    let mut packet_count: u64 = 0;

    loop {
        let packet = match format_reader.next_packet() {
            Ok(Some(p)) => p,
            Ok(None) => break,
            Err(SymphoniaError::ResetRequired) => {
                decoder.reset();
                continue;
            }
            Err(SymphoniaError::IoError(ref e))
                if e.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break;
            }
            Err(e) => {
                tracing::error!(error = %e, "Read error");
                break;
            }
        };

        if packet.track_id() != track_id {
            continue;
        }

        match decoder.decode(&packet) {
            Ok(decoded) => {
                let frames = decoded.samples_interleaved();
                let ch = decoded.spec().channels().count();
                if ch > 0 {
                    total_frames += (frames / ch) as u64;
                }
                packet_count += 1;
            }
            Err(SymphoniaError::DecodeError(_)) => continue,
            Err(SymphoniaError::ResetRequired) => {
                decoder.reset();
                continue;
            }
            Err(e) => {
                tracing::error!(error = %e, "Decode error");
                break;
            }
        }
    }

    #[expect(clippy::cast_precision_loss)] // frame count precision loss is negligible for duration
    let duration_secs = if sample_rate > 0 {
        total_frames as f64 / f64::from(sample_rate)
    } else {
        0.0
    };

    info!(
        codec = %codec_name,
        sample_rate,
        channels,
        duration_secs = format!("{duration_secs:.2}"),
        total_frames,
        packets = packet_count,
        "Decode complete"
    );
}

fn main() {
    init_tracing();

    info!("=== Kithara Decode Demo ===");

    #[cfg(not(target_arch = "wasm32"))]
    {
        native_main();
    }

    #[cfg(target_arch = "wasm32")]
    {
        wasm_main();
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn native_main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        tracing::error!("Usage: decode_demo <audio-file>");
        return;
    }

    let path = &args[1];
    info!(path, "Reading file");

    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(e) => {
            tracing::error!(error = %e, path, "Failed to read file");
            return;
        }
    };

    let filename = std::path::Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(path);

    decode_file(data, filename);
}

#[cfg(target_arch = "wasm32")]
fn wasm_main() {
    use wasm_bindgen::{JsCast, prelude::Closure};

    let window = web_sys::window().expect_throw("no global window");
    let document = window.document().expect_throw("no document");

    // Find or create the file input element.
    let input: web_sys::HtmlInputElement = document
        .get_element_by_id("audio-file-input")
        .and_then(|el| el.dyn_into::<web_sys::HtmlInputElement>().ok())
        .unwrap_or_else(|| {
            let el = document
                .create_element("input")
                .expect_throw("create input")
                .dyn_into::<web_sys::HtmlInputElement>()
                .expect_throw("cast input");
            el.set_type("file");
            el.set_accept("audio/*");
            el.set_id("audio-file-input");
            document
                .body()
                .expect_throw("no body")
                .append_child(&el)
                .expect_throw("append input");
            el
        });

    // Attach change listener.
    let closure = Closure::<dyn Fn(web_sys::Event)>::new(move |event: web_sys::Event| {
        let target = event.target().expect_throw("no target");
        let input: web_sys::HtmlInputElement = target.dyn_into().expect_throw("cast input");
        let files = input.files().expect_throw("no files");

        let Some(file) = files.get(0) else {
            return;
        };

        let filename = file.name();
        info!(filename = %filename, "File selected");

        // Read file bytes via arrayBuffer().
        let promise = file.array_buffer();
        let future = wasm_bindgen_futures::JsFuture::from(promise);

        wasm_bindgen_futures::spawn_local(async move {
            match future.await {
                Ok(buffer) => {
                    let uint8 = js_sys::Uint8Array::new(&buffer);
                    let data = uint8.to_vec();
                    decode_file(data, &filename);
                }
                Err(e) => {
                    tracing::error!(error = ?e, "Failed to read file bytes");
                }
            }
        });
    });

    input
        .add_event_listener_with_callback("change", closure.as_ref().unchecked_ref())
        .expect_throw("add listener");

    // Prevent closure from being dropped (leak is intentional — lives for page lifetime).
    closure.forget();

    info!("Ready — select an audio file to decode");
}

/// Equivalent of `unwrap()` that works in both targets.
/// Uses `expect` on native, `expect_throw` on WASM (avoids `unwrap_used` lint).
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::UnwrapThrowExt;

fn init_tracing() {
    #[cfg(not(target_arch = "wasm32"))]
    {
        tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
            )
            .init();
    }

    #[cfg(target_arch = "wasm32")]
    {
        console_error_panic_hook::set_once();
        tracing_wasm::set_as_global_default();
    }
}
