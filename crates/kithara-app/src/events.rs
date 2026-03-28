use std::time::Duration;

use kithara::prelude::{AudioEvent, Event, FileEvent, HlsEvent};

#[must_use]
pub fn is_progress_event(event: &Event) -> bool {
    matches!(
        event,
        Event::Audio(AudioEvent::PlaybackProgress { .. })
            | Event::File(FileEvent::DownloadProgress { .. })
            | Event::File(FileEvent::ByteProgress { .. })
            | Event::File(FileEvent::PlaybackProgress { .. })
            | Event::Hls(HlsEvent::DownloadProgress { .. })
            | Event::Hls(HlsEvent::ByteProgress { .. })
            | Event::Hls(HlsEvent::PlaybackProgress { .. })
    )
}

#[must_use]
pub fn source_note(source: &str, event: &Event) -> Option<String> {
    match event {
        Event::Audio(AudioEvent::FormatDetected { spec }) => Some(format!(
            "{source} fmt {}ch {}Hz",
            spec.channels, spec.sample_rate
        )),
        Event::Audio(AudioEvent::SeekComplete { position, .. }) => Some(format!(
            "{source} seek {}",
            format_seconds(position.as_secs_f64())
        )),
        Event::Hls(HlsEvent::VariantApplied {
            to_variant, reason, ..
        }) => Some(format!("{source} abr v{to_variant} {reason:?}")),
        Event::Audio(AudioEvent::DecoderReady {
            base_offset,
            variant,
        }) => Some(format!(
            "{source} decoder ready offset={base_offset} v{v}",
            v = variant.map_or("?".to_string(), |v| v.to_string())
        )),
        Event::File(FileEvent::DownloadComplete { total_bytes }) => {
            Some(format!("{source} dl done {total_bytes} bytes"))
        }
        _ => None,
    }
}

const SECONDS_PER_MINUTE: u64 = 60;

#[must_use]
pub fn format_seconds(seconds: f64) -> String {
    let whole = if seconds.is_finite() && seconds > 0.0 {
        Duration::from_secs_f64(seconds).as_secs()
    } else {
        0
    };
    let minutes = whole / SECONDS_PER_MINUTE;
    let secs = whole % SECONDS_PER_MINUTE;
    format!("{minutes:02}:{secs:02}")
}
