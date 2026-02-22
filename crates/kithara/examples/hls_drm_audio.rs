//! Play audio from an AES-128 encrypted HLS stream.
//!
//! ```
//! cargo run -p kithara --example hls_drm_audio --features rodio [URL]
//! ```
//!
//! Controls:
//! - Left/Right arrows: seek ±5 seconds
//! - Up/Down arrows: volume ±5%
//! - Ctrl+C: stop playback

use std::{env::args, error::Error, io, io::Write, sync::mpsc, thread, time::Duration};

use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    terminal::{disable_raw_mode, enable_raw_mode},
};
use kithara::prelude::*;
use rodio::{OutputStreamBuilder, Sink};
use tokio::sync::broadcast::error::RecvError;
use tracing::{info, metadata::LevelFilter, warn};
use tracing_subscriber::EnvFilter;
use url::Url;

const CONTROL_POLL_MS: u64 = 100;
const SEEK_STEP_SECONDS: i64 = 5;
const VOLUME_STEP: f32 = 0.05;

#[derive(Clone, Copy)]
enum ControlCmd {
    SeekBy(i64),
    VolumeBy(f32),
}

struct RawModeGuard;

impl RawModeGuard {
    fn new() -> Result<Self, Box<dyn Error + Send + Sync>> {
        enable_raw_mode()?;
        Ok(Self)
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
    }
}

struct CrlfWriter<W> {
    inner: W,
}

impl<W> CrlfWriter<W> {
    fn new(inner: W) -> Self {
        Self { inner }
    }
}

impl<W: Write> Write for CrlfWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if !buf.contains(&b'\n') {
            self.inner.write_all(buf)?;
            return Ok(buf.len());
        }

        let mut out = Vec::with_capacity(buf.len() + 8);
        for byte in buf {
            if *byte == b'\n' {
                out.push(b'\r');
            }
            out.push(*byte);
        }

        self.inner.write_all(&out)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

fn make_log_writer() -> CrlfWriter<io::Stderr> {
    CrlfWriter::new(io::stderr())
}

fn apply_control(sink: &Sink, cmd: ControlCmd) {
    match cmd {
        ControlCmd::SeekBy(delta_secs) => {
            let delta = Duration::from_secs(delta_secs.unsigned_abs());
            let current = sink.get_pos();
            let target = if delta_secs.is_negative() {
                current.saturating_sub(delta)
            } else {
                current.saturating_add(delta)
            };

            if let Err(err) = sink.try_seek(target) {
                warn!(?err, ?target, "seek failed");
            } else {
                info!(?target, "seek updated");
            }
        }
        ControlCmd::VolumeBy(delta) => {
            let volume = (sink.volume() + delta).clamp(0.0, 1.0);
            sink.set_volume(volume);
            info!(volume, "volume updated");
        }
    }
}

#[tokio::main(flavor = "current_thread")]
#[cfg_attr(feature = "perf", hotpath::main)]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::default()
                .add_directive("kithara_audio=debug".parse()?)
                .add_directive("kithara_net=warn".parse()?)
                .add_directive("symphonia_format_isomp4=warn".parse()?)
                .add_directive(LevelFilter::INFO.into()),
        )
        .with_writer(make_log_writer)
        .with_line_number(false)
        .with_file(false)
        .init();

    let url: Url = args()
        .nth(1)
        .unwrap_or_else(|| "https://stream.silvercomet.top/drm/master.m3u8".to_string())
        .parse()?;

    info!("Opening encrypted HLS stream: {url}");

    let bus = EventBus::new(128);
    let mut events_rx = bus.subscribe();
    let pool = ThreadPool::with_num_threads(2)?;
    let hls_config = HlsConfig::new(url)
        .with_thread_pool(pool)
        .with_events(bus)
        .with_abr(AbrOptions {
            mode: AbrMode::Auto(Some(0)),
            ..Default::default()
        });
    let config = AudioConfig::<Hls>::new(hls_config).with_prefer_hardware(true);
    let audio = Audio::<Stream<Hls>>::new(config).await?;

    info!("Starting playback... (Press Ctrl+C to stop)");

    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    let (control_tx, control_rx) = mpsc::channel::<ControlCmd>();
    let (keyboard_stop_tx, keyboard_stop_rx) = mpsc::channel::<()>();
    let mut playback = tokio::task::spawn_blocking(move || {
        let out = OutputStreamBuilder::open_default_stream()?;
        let sink = Sink::connect_new(out.mixer());
        sink.append(audio);
        info!("Playing...");

        while stop_rx.try_recv().is_err() && !sink.empty() {
            while let Ok(cmd) = control_rx.try_recv() {
                apply_control(&sink, cmd);
            }
            thread::sleep(Duration::from_millis(CONTROL_POLL_MS));
        }
        sink.stop();
        info!("Playback stopped");
        Ok::<_, Box<dyn Error + Send + Sync>>(())
    });

    let keyboard = tokio::task::spawn_blocking(move || {
        let _raw = match RawModeGuard::new() {
            Ok(raw) => raw,
            Err(err) => {
                warn!(?err, "keyboard controls unavailable");
                return Ok(());
            }
        };

        info!(
            "Controls: Left/Right seek ±{}s, Up/Down volume ±{}%",
            SEEK_STEP_SECONDS,
            (VOLUME_STEP * 100.0).round()
        );

        loop {
            if keyboard_stop_rx.try_recv().is_ok() {
                break;
            }
            if !event::poll(Duration::from_millis(CONTROL_POLL_MS))? {
                continue;
            }

            let Event::Key(key) = event::read()? else {
                continue;
            };
            if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
                continue;
            }

            let cmd = match key.code {
                KeyCode::Left => Some(ControlCmd::SeekBy(-SEEK_STEP_SECONDS)),
                KeyCode::Right => Some(ControlCmd::SeekBy(SEEK_STEP_SECONDS)),
                KeyCode::Up => Some(ControlCmd::VolumeBy(VOLUME_STEP)),
                KeyCode::Down => Some(ControlCmd::VolumeBy(-VOLUME_STEP)),
                _ => None,
            };

            if let Some(cmd) = cmd
                && control_tx.send(cmd).is_err()
            {
                break;
            }
        }

        Ok::<_, Box<dyn Error + Send + Sync>>(())
    });
    let mut playback_finished = false;

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                info!("Ctrl+C received, stopping...");
                break;
            }
            recv = events_rx.recv() => {
                match recv {
                    Ok(ev) => info!(?ev),
                    Err(RecvError::Lagged(n)) => warn!(n, "events lagged"),
                    Err(_) => break,
                }
            }
            result = &mut playback => {
                result??;
                playback_finished = true;
                break;
            }
        }
    }

    let _ = stop_tx.send(());
    let _ = keyboard_stop_tx.send(());
    if !playback_finished {
        playback.await??;
    }
    keyboard.await??;

    Ok(())
}
