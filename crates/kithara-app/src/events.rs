use kithara::{
    events::DownloaderEvent,
    prelude::{AudioEvent, Event, FileEvent, HlsEvent},
};
use kithara_platform::time::Duration;

#[must_use]
pub fn is_progress_event(event: &Event) -> bool {
    matches!(
        event,
        Event::Audio(AudioEvent::PlaybackProgress { .. })
            | Event::File(FileEvent::ReadProgress { .. })
            | Event::Hls(HlsEvent::ReadProgress { .. })
            | Event::Downloader(DownloaderEvent::RequestCompleted { .. })
    )
}

#[must_use]
pub fn format_seconds(seconds: f64) -> String {
    const SECONDS_PER_MINUTE: u64 = 60;
    let whole = if seconds.is_finite() && seconds > 0.0 {
        Duration::from_secs_f64(seconds).as_secs()
    } else {
        0
    };
    let minutes = whole / SECONDS_PER_MINUTE;
    let secs = whole % SECONDS_PER_MINUTE;
    format!("{minutes:02}:{secs:02}")
}
