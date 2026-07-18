#![forbid(unsafe_code)]

#[cfg(feature = "abr")]
use crate::AbrEvent;
#[cfg(feature = "app")]
use crate::AppEvent;
#[cfg(feature = "asset")]
use crate::AssetEvent;
#[cfg(feature = "audio")]
use crate::AudioEvent;
use crate::BusEvent;
#[cfg(feature = "decoder")]
use crate::DecoderEvent;
#[cfg(feature = "downloader")]
use crate::DownloaderEvent;
#[cfg(feature = "drm")]
use crate::DrmEvent;
#[cfg(feature = "file")]
use crate::FileEvent;
#[cfg(feature = "hls")]
use crate::HlsEvent;
#[cfg(feature = "queue")]
use crate::QueueEvent;
#[cfg(feature = "player")]
use crate::{
    DjEvent, EngineEvent, ItemEvent, PlayerEvent, SessionEvent, SyncEvent, TransportEvent,
};

/// Unified event for the full audio pipeline.
///
/// Hierarchical: each subsystem has its own variant with a sub-enum.
/// All variants are feature-gated.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum Event {
    /// Bus-level synthetic event.
    Bus(BusEvent),
    /// Unified downloader event (soft-timeout, progress, completion,
    /// error). Published by the downloader layer on the peer's bus.
    #[cfg(feature = "downloader")]
    Downloader(DownloaderEvent),
    /// HLS stream event.
    #[cfg(feature = "hls")]
    Hls(HlsEvent),
    /// File stream event.
    #[cfg(feature = "file")]
    File(FileEvent),
    /// Audio pipeline event.
    #[cfg(feature = "audio")]
    Audio(AudioEvent),
    /// Decoder lifecycle event.
    #[cfg(feature = "decoder")]
    Decoder(DecoderEvent),
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
    /// Session transport event.
    #[cfg(feature = "player")]
    Transport(TransportEvent),
    /// Track synchronization event.
    #[cfg(feature = "player")]
    Sync(SyncEvent),
    /// DJ feature event.
    #[cfg(feature = "player")]
    Dj(DjEvent),
    /// Application lifecycle event.
    #[cfg(feature = "app")]
    App(AppEvent),
    /// Asset cache event.
    #[cfg(feature = "asset")]
    Asset(AssetEvent),
    /// Queue-level event (track added/removed/status/current/ended).
    #[cfg(feature = "queue")]
    Queue(QueueEvent),
    /// DRM lifecycle event.
    #[cfg(feature = "drm")]
    Drm(DrmEvent),
    /// ABR controller event.
    #[cfg(feature = "abr")]
    Abr(AbrEvent),
}

impl From<BusEvent> for Event {
    fn from(e: BusEvent) -> Self {
        Self::Bus(e)
    }
}

#[cfg(feature = "downloader")]
impl From<DownloaderEvent> for Event {
    fn from(e: DownloaderEvent) -> Self {
        Self::Downloader(e)
    }
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

#[cfg(feature = "decoder")]
impl From<DecoderEvent> for Event {
    fn from(e: DecoderEvent) -> Self {
        Self::Decoder(e)
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
impl From<TransportEvent> for Event {
    fn from(e: TransportEvent) -> Self {
        Self::Transport(e)
    }
}

#[cfg(feature = "player")]
impl From<SyncEvent> for Event {
    fn from(e: SyncEvent) -> Self {
        Self::Sync(e)
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

#[cfg(feature = "asset")]
impl From<AssetEvent> for Event {
    fn from(e: AssetEvent) -> Self {
        Self::Asset(e)
    }
}

#[cfg(feature = "queue")]
impl From<QueueEvent> for Event {
    fn from(e: QueueEvent) -> Self {
        Self::Queue(e)
    }
}

#[cfg(feature = "drm")]
impl From<DrmEvent> for Event {
    fn from(e: DrmEvent) -> Self {
        Self::Drm(e)
    }
}

#[cfg(feature = "abr")]
impl From<AbrEvent> for Event {
    fn from(e: AbrEvent) -> Self {
        Self::Abr(e)
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    fn file_is_end_of_stream(event: &FileEvent) -> bool {
        matches!(event, FileEvent::EndOfStream)
    }

    fn file_is_read_progress_42(event: &FileEvent) -> bool {
        matches!(
            event,
            FileEvent::ReadProgress {
                position: 42,
                total: None,
            }
        )
    }

    #[kithara::test]
    #[case(FileEvent::EndOfStream, file_is_end_of_stream)]
    #[case(
        FileEvent::ReadProgress { position: 42, total: None },
        file_is_read_progress_42
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
    #[kithara::test]
    #[case(HlsEvent::EndOfStream, hls_is_end_of_stream)]
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

    #[cfg(feature = "decoder")]
    #[kithara::test]
    fn decoder_event_into_event() {
        let event: Event = DecoderEvent::DecodeError {
            class: crate::DecodeErrorClass::Other,
            kind: crate::DecodeErrorKind::InvalidData,
            codec: None,
            detail: "invalid data",
        }
        .into();
        assert!(matches!(
            event,
            Event::Decoder(DecoderEvent::DecodeError {
                class: crate::DecodeErrorClass::Other,
                kind: crate::DecodeErrorKind::InvalidData,
                codec: None,
                detail: "invalid data",
            })
        ));
    }

    #[cfg(feature = "asset")]
    #[kithara::test]
    fn asset_event_into_event() {
        let event: Event = AssetEvent::Evicted {
            asset_root: "root".to_string(),
            reason: crate::EvictReason::Displaced,
        }
        .into();
        assert!(matches!(
            event,
            Event::Asset(AssetEvent::Evicted {
                asset_root,
                reason: crate::EvictReason::Displaced,
            }) if asset_root == "root"
        ));
    }

    #[cfg(feature = "drm")]
    #[kithara::test]
    fn drm_event_into_event() {
        let event: Event = DrmEvent::KeyFetchFailed {
            key_host: Some("example.com".to_string()),
            stage: crate::KeyFailureStage::Network,
            detail: "network failed".to_string(),
        }
        .into();
        assert!(matches!(
            event,
            Event::Drm(DrmEvent::KeyFetchFailed {
                key_host: Some(host),
                stage: crate::KeyFailureStage::Network,
                detail,
            }) if host == "example.com" && detail == "network failed"
        ));
    }

    #[cfg(feature = "player")]
    #[kithara::test]
    fn transport_event_into_event() {
        let event: Event = TransportEvent::TempoCommitted {
            beats_per_minute: 120.0,
            revision: 3,
        }
        .into();

        assert!(matches!(
            event,
            Event::Transport(TransportEvent::TempoCommitted {
                beats_per_minute: 120.0,
                revision: 3,
            })
        ));
    }

    #[cfg(feature = "player")]
    #[kithara::test]
    fn sync_event_into_event() {
        let event: Event = SyncEvent::BindingCommitted {
            slot: crate::SlotId::new(2),
            session_anchor_beats: 8.0,
            track_anchor_beats: 16.0,
            direction: crate::PlaybackDirection::Reverse,
        }
        .into();

        assert!(matches!(
            event,
            Event::Sync(SyncEvent::BindingCommitted {
                slot,
                session_anchor_beats: 8.0,
                track_anchor_beats: 16.0,
                direction: crate::PlaybackDirection::Reverse,
            }) if slot == crate::SlotId::new(2)
        ));
    }
}
