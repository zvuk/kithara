#![forbid(unsafe_code)]

#[cfg(feature = "audio")]
use crate::AudioEvent;
use crate::FileEvent;
#[cfg(feature = "hls")]
use crate::HlsEvent;

/// Unified event for the full audio pipeline.
///
/// Hierarchical: each subsystem has its own variant with a sub-enum.
#[derive(Clone, Debug)]
pub enum Event {
    /// HLS stream event.
    #[cfg(feature = "hls")]
    Hls(HlsEvent),
    /// File stream event.
    File(FileEvent),
    /// Audio pipeline event.
    #[cfg(feature = "audio")]
    Audio(AudioEvent),
}

#[cfg(feature = "hls")]
impl From<HlsEvent> for Event {
    fn from(e: HlsEvent) -> Self {
        Self::Hls(e)
    }
}

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

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    fn file_is_end_of_stream(event: &FileEvent) -> bool {
        matches!(event, FileEvent::EndOfStream)
    }

    fn file_is_download_complete_42(event: &FileEvent) -> bool {
        matches!(event, FileEvent::DownloadComplete { total_bytes: 42 })
    }

    #[rstest]
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
    #[rstest]
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
    #[test]
    fn audio_event_into_event() {
        let event: Event = AudioEvent::EndOfStream.into();
        assert!(matches!(event, Event::Audio(AudioEvent::EndOfStream)));
    }
}
