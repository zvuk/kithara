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
    use super::*;

    #[test]
    fn file_event_into_event() {
        let event: Event = FileEvent::EndOfStream.into();
        assert!(matches!(event, Event::File(FileEvent::EndOfStream)));
    }

    #[test]
    fn file_event_download_complete() {
        let event: Event = FileEvent::DownloadComplete { total_bytes: 42 }.into();
        assert!(matches!(
            event,
            Event::File(FileEvent::DownloadComplete { total_bytes: 42 })
        ));
    }

    #[cfg(feature = "hls")]
    #[test]
    fn hls_event_into_event() {
        let event: Event = HlsEvent::EndOfStream.into();
        assert!(matches!(event, Event::Hls(HlsEvent::EndOfStream)));
    }

    #[cfg(feature = "hls")]
    #[test]
    fn hls_event_variant_applied() {
        use kithara_abr::AbrReason;

        let hls = HlsEvent::VariantApplied {
            from_variant: 0,
            to_variant: 1,
            reason: AbrReason::UpSwitch,
        };
        let event: Event = hls.into();
        match event {
            Event::Hls(HlsEvent::VariantApplied {
                from_variant,
                to_variant,
                reason,
            }) => {
                assert_eq!(from_variant, 0);
                assert_eq!(to_variant, 1);
                assert!(matches!(reason, AbrReason::UpSwitch));
            }
            _ => panic!("expected Hls(VariantApplied)"),
        }
    }

    #[cfg(feature = "audio")]
    #[test]
    fn audio_event_into_event() {
        let event: Event = AudioEvent::EndOfStream.into();
        assert!(matches!(event, Event::Audio(AudioEvent::EndOfStream)));
    }
}
