use std::time::Duration;

#[cfg(feature = "hls")]
use kithara::prelude::HlsEvent;
use kithara::prelude::{AudioEvent, Event, FileEvent};
use ratatui::{
    Frame,
    layout::{Constraint, Layout},
    style::{Color, Style},
    text::Line,
    widgets::{Clear, Paragraph},
};

pub(crate) const DASHBOARD_HEIGHT: u16 = 1;

const NOTE_MAX_CHARS: usize = 24;
const MIN_PROGRESS_BAR_WIDTH: usize = 4;
const SEGMENT_COUNT: usize = 6;

pub(crate) struct Dashboard {
    abr_reason: Option<String>,
    abr_variant: Option<usize>,
    byte_position: Option<u64>,
    byte_total: Option<u64>,
    download_offset: Option<u64>,
    download_total: Option<u64>,
    last_note: Option<String>,
    last_throughput_bps: Option<f64>,
    position_ms: u64,
    source_label: &'static str,
    total_ms: Option<u64>,
    track: String,
    volume: f32,
}

impl Dashboard {
    pub(crate) fn new(source_label: &'static str, track: String, volume: f32) -> Self {
        Self {
            abr_reason: None,
            abr_variant: None,
            byte_position: None,
            byte_total: None,
            download_offset: None,
            download_total: None,
            last_note: None,
            last_throughput_bps: None,
            position_ms: 0,
            source_label,
            total_ms: None,
            track,
            volume,
        }
    }

    pub(crate) fn progress_log_line(&self) -> String {
        format!(
            "progress {}/{} rd {}/{} dl {}/{} vol {:.0}% note={}",
            format_ms(self.position_ms),
            self.total_ms.map_or_else(|| "--:--".to_string(), format_ms),
            self.byte_position
                .map_or_else(|| "-".to_string(), format_bytes),
            self.byte_total
                .map_or_else(|| "?".to_string(), format_bytes),
            self.download_offset
                .map_or_else(|| "-".to_string(), format_bytes),
            self.download_total
                .map_or_else(|| "?".to_string(), format_bytes),
            self.volume * 100.0,
            self.last_note.as_deref().unwrap_or("-")
        )
    }

    pub(crate) fn push_note(&mut self, note: impl Into<String>) {
        self.last_note = Some(note.into());
    }

    pub(crate) fn render(&self, frame: &mut Frame) {
        let chunks = Layout::horizontal([
            Constraint::Percentage(23),
            Constraint::Percentage(24),
            Constraint::Percentage(19),
            Constraint::Percentage(14),
            Constraint::Percentage(8),
            Constraint::Percentage(12),
        ])
        .split(frame.area());

        let progress_bar_width = usize::from(chunks[1].width)
            .saturating_sub(14)
            .max(MIN_PROGRESS_BAR_WIDTH);
        let segments = [
            format!("▌ {} ♫{}", self.source_label, self.track),
            format!(
                "▶{}/{} {}",
                format_ms(self.position_ms),
                self.total_ms.map_or_else(|| "--:--".to_string(), format_ms),
                self.progress_bar(progress_bar_width)
            ),
            format!(
                "↧{}/{} ↯{}",
                self.byte_position
                    .map_or_else(|| "-".to_string(), format_bytes),
                self.byte_total
                    .map_or_else(|| "?".to_string(), format_bytes),
                self.buffered_bytes()
                    .map_or_else(|| "-".to_string(), format_bytes)
            ),
            format!(
                "⇅v{} {}",
                self.abr_variant
                    .map_or_else(|| "-".to_string(), |v| v.to_string()),
                self.last_throughput_bps
                    .map_or_else(|| "-".to_string(), format_bps)
            ),
            format!("🔉{:>3.0}%", self.volume * 100.0),
            clamp_text(self.last_note.as_deref().unwrap_or("-"), NOTE_MAX_CHARS),
        ];

        let style = Style::default().fg(Color::White).bg(Color::Rgb(20, 24, 30));
        frame.render_widget(Clear, frame.area());
        for (index, chunk) in chunks.iter().enumerate() {
            if index >= SEGMENT_COUNT {
                break;
            }
            let text = fit_cell(&segments[index], usize::from(chunk.width));
            let widget = Paragraph::new(Line::raw(text)).style(style);
            frame.render_widget(widget, *chunk);
        }
    }

    pub(crate) fn set_volume(&mut self, volume: f32) {
        self.volume = volume.clamp(0.0, 1.0);
    }

    pub(crate) fn set_position(&mut self, position: Duration) {
        self.position_ms = u64::try_from(position.as_millis()).unwrap_or(u64::MAX);
    }

    pub(crate) fn set_total_duration(&mut self, total: Option<Duration>) {
        self.total_ms =
            total.map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX));
    }

    pub(crate) fn track_event(&mut self, event: &Event) {
        match event {
            Event::Audio(AudioEvent::PlaybackProgress {
                position_ms,
                total_ms,
                ..
            }) => {
                self.position_ms = *position_ms;
                self.total_ms = *total_ms;
            }
            Event::Audio(AudioEvent::FormatDetected { spec }) => {
                self.last_note = Some(format!("fmt {}ch {}Hz", spec.channels, spec.sample_rate));
            }
            Event::Audio(AudioEvent::FormatChanged { old, new }) => {
                self.last_note = Some(format!(
                    "fmt {}Hz->{}Hz {}ch->{}ch",
                    old.sample_rate, new.sample_rate, old.channels, new.channels
                ));
            }
            Event::Audio(AudioEvent::SeekLifecycle { stage, .. }) => {
                self.last_note = Some(format!("seek {:?}", stage));
            }
            Event::Audio(AudioEvent::SeekComplete { position, .. }) => {
                self.set_position(*position);
                self.last_note = Some(format!("seek {}", format_duration(*position)));
            }
            Event::File(FileEvent::DownloadProgress { offset, total }) => {
                self.download_offset = Some(*offset);
                self.download_total = *total;
            }
            Event::File(FileEvent::ByteProgress { position, total })
            | Event::File(FileEvent::PlaybackProgress { position, total }) => {
                self.byte_position = Some(*position);
                self.byte_total = *total;
            }
            Event::File(FileEvent::DownloadComplete { total_bytes }) => {
                self.last_note = Some(format!("file dl done {}", format_bytes(*total_bytes)));
            }
            Event::File(FileEvent::DownloadError { error }) => {
                self.last_note = Some(format!("file dl err {}", clamp_text(error, 28)));
            }
            Event::File(FileEvent::Error { error, recoverable }) => {
                self.last_note = Some(format!(
                    "file err rec={} {}",
                    recoverable,
                    clamp_text(error, 24)
                ));
            }
            Event::File(FileEvent::EndOfStream) => {
                self.last_note = Some("file eos".to_string());
            }
            #[cfg(feature = "hls")]
            Event::Hls(HlsEvent::VariantApplied {
                to_variant, reason, ..
            }) => {
                self.abr_variant = Some(*to_variant);
                self.abr_reason = Some(format!("{reason:?}"));
                self.last_note = Some(format!("abr v={} {:?}", to_variant, reason));
            }
            #[cfg(feature = "hls")]
            Event::Hls(HlsEvent::ThroughputSample { bytes_per_second }) => {
                self.last_throughput_bps = Some(*bytes_per_second);
            }
            #[cfg(feature = "hls")]
            Event::Hls(HlsEvent::DownloadProgress { offset, total }) => {
                self.download_offset = Some(*offset);
                self.download_total = *total;
            }
            #[cfg(feature = "hls")]
            Event::Hls(HlsEvent::ByteProgress { position, total })
            | Event::Hls(HlsEvent::PlaybackProgress { position, total }) => {
                self.byte_position = Some(*position);
                self.byte_total = *total;
            }
            #[cfg(feature = "hls")]
            Event::Hls(HlsEvent::SegmentComplete {
                variant,
                segment_index,
                cached,
                ..
            }) => {
                self.last_note = Some(format!(
                    "seg v{}#{} cached={}",
                    variant, segment_index, cached
                ));
            }
            #[cfg(feature = "hls")]
            Event::Hls(HlsEvent::DownloadError { error }) => {
                self.last_note = Some(format!("hls dl err {}", clamp_text(error, 28)));
            }
            #[cfg(feature = "hls")]
            Event::Hls(HlsEvent::Error { error, recoverable }) => {
                self.last_note = Some(format!(
                    "hls err rec={} {}",
                    recoverable,
                    clamp_text(error, 24)
                ));
            }
            #[cfg(feature = "hls")]
            Event::Hls(HlsEvent::EndOfStream) => {
                self.last_note = Some("hls eos".to_string());
            }
            _ => {}
        }
    }

    pub(crate) fn track_seek(&mut self, target: Duration) {
        self.last_note = Some(format!("seek {}", format_duration(target)));
    }

    fn buffered_bytes(&self) -> Option<u64> {
        let downloaded = self.download_offset?;
        let consumed = self.byte_position?;
        Some(downloaded.saturating_sub(consumed))
    }

    fn progress_bar(&self, width: usize) -> String {
        if width == 0 {
            return String::new();
        }
        let Some(total_ms) = self.total_ms else {
            return "▱".repeat(width);
        };
        if total_ms == 0 {
            return "▱".repeat(width);
        }
        let width_u64 = u64::try_from(width).unwrap_or(u64::MAX);
        let filled_u64 = self.position_ms.min(total_ms).saturating_mul(width_u64) / total_ms;
        let filled = usize::try_from(filled_u64).unwrap_or(width).min(width);
        format!(
            "{}{}",
            "▰".repeat(filled),
            "▱".repeat(width.saturating_sub(filled))
        )
    }
}

fn format_bps(bytes_per_second: f64) -> String {
    if bytes_per_second >= 1_048_576.0 {
        format!("{:.2} MiB/s", bytes_per_second / 1_048_576.0)
    } else if bytes_per_second >= 1024.0 {
        format!("{:.2} KiB/s", bytes_per_second / 1024.0)
    } else {
        format!("{bytes_per_second:.0} B/s")
    }
}

fn format_bytes(bytes: u64) -> String {
    if bytes >= 1_048_576 {
        let whole = bytes / 1_048_576;
        let frac = (bytes % 1_048_576) * 100 / 1_048_576;
        format!("{whole}.{frac:02} MiB")
    } else if bytes >= 1024 {
        let whole = bytes / 1024;
        let frac = (bytes % 1024) * 100 / 1024;
        format!("{whole}.{frac:02} KiB")
    } else {
        format!("{bytes} B")
    }
}

fn format_duration(duration: Duration) -> String {
    let millis = u64::try_from(duration.as_millis()).unwrap_or(u64::MAX);
    format_ms(millis)
}

fn format_ms(ms: u64) -> String {
    let total_seconds = ms / 1000;
    let minutes = total_seconds / 60;
    let seconds = total_seconds % 60;
    format!("{minutes:02}:{seconds:02}")
}

fn clamp_text(text: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    let count = text.chars().count();
    if count <= max_chars {
        return text.to_string();
    }
    if max_chars <= 3 {
        return text.chars().take(max_chars).collect();
    }
    let keep = max_chars - 3;
    let mut out: String = text.chars().take(keep).collect();
    out.push_str("...");
    out
}

fn fit_cell(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let mut out = clamp_text(text, width);
    let used = out.chars().count();
    if used < width {
        out.push_str(&" ".repeat(width - used));
    }
    out
}
