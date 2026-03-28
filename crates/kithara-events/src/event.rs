#![forbid(unsafe_code)]

#[cfg(feature = "app")]
use crate::AppEvent;
#[cfg(feature = "audio")]
use crate::AudioEvent;
#[cfg(feature = "file")]
use crate::FileEvent;
#[cfg(feature = "hls")]
use crate::HlsEvent;
#[cfg(feature = "player")]
use crate::{DjEvent, EngineEvent, ItemEvent, PlayerEvent, SessionEvent};

/// Unified event for the full audio pipeline.
///
/// Hierarchical: each subsystem has its own variant with a sub-enum.
/// All variants are feature-gated.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum Event {
    /// HLS stream event.
    #[cfg(feature = "hls")]
    Hls(HlsEvent),
    /// File stream event.
    #[cfg(feature = "file")]
    File(FileEvent),
    /// Audio pipeline event.
    #[cfg(feature = "audio")]
    Audio(AudioEvent),
    /// Player state event.
    #[cfg(feature = "player")]
    Player(PlayerEvent),
    /// Engine lifecycle event.
    #[cfg(feature = "player")]
    Engine(EngineEvent),
    /// Item state event.
    #[cfg(feature = "player")]
    Item(ItemEvent),
    /// Audio session event.
    #[cfg(feature = "player")]
    Session(SessionEvent),
    /// DJ feature event.
    #[cfg(feature = "player")]
    Dj(DjEvent),
    /// Application lifecycle event.
    #[cfg(feature = "app")]
    App(AppEvent),
}

#[cfg(feature = "hls")]
impl From<HlsEvent> for Event {
    fn from(e: HlsEvent) -> Self {
        Self::Hls(e)
    }
}

#[cfg(feature = "file")]
impl From<FileEvent> for Event {
    fn from(e: FileEvent) -> Self {
        Self::File(e)
    }
}

#[cfg(feature = "audio")]
impl From<AudioEvent> for Event {
    fn from(e: AudioEvent) -> Self {
        Self::Audio(e)
    }
}

#[cfg(feature = "player")]
impl From<PlayerEvent> for Event {
    fn from(e: PlayerEvent) -> Self {
        Self::Player(e)
    }
}

#[cfg(feature = "player")]
impl From<EngineEvent> for Event {
    fn from(e: EngineEvent) -> Self {
        Self::Engine(e)
    }
}

#[cfg(feature = "player")]
impl From<ItemEvent> for Event {
    fn from(e: ItemEvent) -> Self {
        Self::Item(e)
    }
}

#[cfg(feature = "player")]
impl From<SessionEvent> for Event {
    fn from(e: SessionEvent) -> Self {
        Self::Session(e)
    }
}

#[cfg(feature = "player")]
impl From<DjEvent> for Event {
    fn from(e: DjEvent) -> Self {
        Self::Dj(e)
    }
}

#[cfg(feature = "app")]
impl From<AppEvent> for Event {
    fn from(e: AppEvent) -> Self {
        Self::App(e)
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    fn file_is_end_of_stream(event: &FileEvent) -> bool {
        matches!(event, FileEvent::EndOfStream)
    }

    fn file_is_download_complete_42(event: &FileEvent) -> bool {
        matches!(event, FileEvent::DownloadComplete { total_bytes: 42 })
    }

    #[kithara::test]
    #[case(FileEvent::EndOfStream, file_is_end_of_stream)]
    #[case(
        FileEvent::DownloadComplete { total_bytes: 42 },
        file_is_download_complete_42
    )]
    fn file_event_into_event(#[case] file_event: FileEvent, #[case] check: fn(&FileEvent) -> bool) {
        let event: Event = file_event.into();
        assert!(matches!(event, Event::File(inner) if check(&inner)));
    }

    #[cfg(feature = "hls")]
    fn hls_is_end_of_stream(event: &HlsEvent) -> bool {
        matches!(event, HlsEvent::EndOfStream)
    }

    #[cfg(feature = "hls")]
    fn hls_is_variant_applied_upswitch(event: &HlsEvent) -> bool {
        matches!(
            event,
            HlsEvent::VariantApplied {
                from_variant: 0,
                to_variant: 1,
                reason: kithara_abr::AbrReason::UpSwitch,
            }
        )
    }

    #[cfg(feature = "hls")]
    #[kithara::test]
    #[case(HlsEvent::EndOfStream, hls_is_end_of_stream)]
    #[case(
        HlsEvent::VariantApplied {
            from_variant: 0,
            to_variant: 1,
            reason: kithara_abr::AbrReason::UpSwitch,
        },
        hls_is_variant_applied_upswitch
    )]
    fn hls_event_into_event(#[case] hls_event: HlsEvent, #[case] check: fn(&HlsEvent) -> bool) {
        let event: Event = hls_event.into();
        assert!(matches!(event, Event::Hls(inner) if check(&inner)));
    }

    #[cfg(feature = "audio")]
    #[kithara::test]
    fn audio_event_into_event() {
        let event: Event = AudioEvent::EndOfStream.into();
        assert!(matches!(event, Event::Audio(AudioEvent::EndOfStream)));
    }
}
