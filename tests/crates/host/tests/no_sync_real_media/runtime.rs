use kithara::{
    audio::{AudioEvent, DecoderEvent, PlaybackResamplerKind},
    download::DownloaderEvent,
    events::{BusEvent, EventReceiver},
    file::FileEvent,
    hls::HlsEvent,
    host::HostOwned,
    platform::{
        sync::{Arc, Mutex},
        tokio::sync::broadcast::error::TryRecvError,
    },
    play::{DeckSnapshot, PlayError, PlayerEvent, SessionError},
    queue::{ItemEvent, Queue},
    signal::{SegmentId, TransportRevision},
};
use kithara_integration_tests::{
    event::TestEvent,
    offline::{OfflineHostHarness, host::ObservedDeck},
};
use kithara_test_utils::bufpool::TestPools;
use serde::Serialize;

use super::{Case, SOURCE_RATE};

pub(super) struct Deck {
    pub(super) player: HostOwned<ObservedDeck<Queue<TestPools>>>,
    pub(super) reference: super::reference::ReferenceAudio,
    pub(super) snapshot: Arc<Mutex<DeckSnapshot>>,
    pub(super) reference_events: EventReceiver<TestEvent>,
    pub(super) events: EventReceiver<TestEvent>,
    pub(super) observation: DeckObservation,
    pub(super) seek_request_segment: Option<SegmentId>,
    pub(super) seek_complete_segment: Option<SegmentId>,
    pub(super) muted_seek_underrun_segment: Option<SegmentId>,
    pub(super) seek_terminal: bool,
    pub(super) capture_target_secs: f64,
}

#[derive(Default, Serialize)]
pub(super) struct DeckObservation {
    pub(super) decoder_changes: usize,
    pub(super) decoder_sample_rates: Vec<u32>,
    pub(super) decoder_channels: Vec<u16>,
    pub(super) decoder_variants: Vec<Option<u32>>,
    pub(super) playback_resamplers: Vec<ResamplerObservation>,
    pub(super) seek_positions_secs: Vec<f64>,
    pub(super) muted_seek_underruns: usize,
    pub(super) final_position_secs: f64,
    pub(super) capture_target_secs: f64,
    pub(super) hls: bool,
    pub(super) label: &'static str,
}

#[derive(Clone, Copy)]
pub(super) enum EventPolicy {
    AudiblePlayback,
    MutedSeekSetup,
}

#[derive(Serialize)]
pub(super) struct ResamplerObservation {
    pub(super) active: bool,
    pub(super) backend: &'static str,
    pub(super) host_sample_rate: u32,
    pub(super) source_sample_rate: u32,
}

pub(super) fn drain_all_events(
    decks: &mut [Deck],
    phase: &str,
    policy: EventPolicy,
    failures: &mut Vec<String>,
) {
    for (deck_index, deck) in decks.iter_mut().enumerate() {
        if let Some(requested) = deck.seek_request_segment {
            let mark = deck.snapshot.lock().slots.iter().find_map(|slot| slot.mark);
            if let Some(mark) = mark {
                if mark.lane.segment == requested && mark.lane.frame > 0 {
                    if deck.seek_complete_segment.is_none() {
                        deck.observation
                            .seek_positions_secs
                            .push(mark.position.as_secs_f64());
                    }
                    deck.seek_complete_segment = Some(mark.lane.segment);
                } else if mark.lane.segment > requested {
                    failures.push(format!("deck {deck_index} committed stale seek segment {:?}, expected {requested:?}", mark.lane.segment));
                    deck.seek_terminal = true;
                }
            }
        }
        loop {
            match deck.events.try_recv() {
                Ok(envelope) => {
                    observe_event(deck_index, deck, envelope.event, phase, policy, failures);
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Lagged(count)) => failures.push(format!(
                    "deck {deck_index} ({}) event receiver lost {count} events during {phase}",
                    deck.observation.label,
                )),
                Err(TryRecvError::Closed) => {
                    failures.push(format!(
                        "deck {deck_index} ({}) event receiver closed during {phase}",
                        deck.observation.label,
                    ));
                    break;
                }
            }
        }
    }
}

/// Fold one event of deck `deck_index` into its observation, or into
/// `failures` when a no-SYNC capture must not see it.
fn observe_event(
    deck_index: usize,
    deck: &mut Deck,
    event: TestEvent,
    phase: &str,
    policy: EventPolicy,
    failures: &mut Vec<String>,
) {
    match event {
        TestEvent::Audio(event) => {
            observe_audio_event(deck_index, deck, &event, phase, policy, failures);
        }
        TestEvent::Decoder(DecoderEvent::DecoderChanged {
            sample_rate,
            channels,
            variant,
            ..
        }) => {
            deck.observation.decoder_changes += 1;
            deck.observation.decoder_sample_rates.push(sample_rate);
            deck.observation.decoder_channels.push(channels);
            deck.observation.decoder_variants.push(variant);
        }
        TestEvent::Decoder(DecoderEvent::DecodeError {
            class,
            kind,
            detail,
            ..
        }) => failures.push(format!(
            "deck {deck_index} ({}) decode error {class:?}/{kind:?} ({detail}) during {phase}",
            deck.observation.label,
        )),
        TestEvent::Player(PlayerEvent::ItemDidPlayToEnd { item }) => {
            deck.seek_terminal = true;
            failures.push(format!(
                "deck {deck_index} ({}) reached player EOF during {phase}",
                item.track(),
            ));
        }
        TestEvent::Player(PlayerEvent::ItemDidFail { .. }) => {
            deck.seek_terminal = true;
            failures.push(format!(
                "deck {deck_index} ({}) reported player track failure during {phase}",
                deck.observation.label,
            ));
        }
        TestEvent::Bus(BusEvent::Overflow { dropped, .. }) => failures.push(format!(
            "deck {deck_index} ({}) event bus dropped {dropped} events during {phase}",
            deck.observation.label,
        )),
        TestEvent::Hls(HlsEvent::Error { error }) => failures.push(format!(
            "deck {deck_index} ({}) HLS error {error:?} during {phase}",
            deck.observation.label,
        )),
        TestEvent::File(FileEvent::Error { error }) => failures.push(format!(
            "deck {deck_index} ({}) file error {error:?} during {phase}",
            deck.observation.label,
        )),
        TestEvent::Downloader(DownloaderEvent::RequestFailed { error, .. }) => {
            failures.push(format!(
                "deck {deck_index} ({}) downloader request failed with {error:?} during {phase}",
                deck.observation.label,
            ));
        }
        TestEvent::Downloader(DownloaderEvent::RetryExhausted { error, .. }) => {
            failures.push(format!(
                "deck {deck_index} ({}) downloader exhausted retries with {error:?} during {phase}",
                deck.observation.label,
            ));
        }
        TestEvent::Item(ItemEvent::PlaybackStalled) => failures.push(format!(
            "deck {deck_index} ({}) playback stalled during {phase}",
            deck.observation.label,
        )),
        TestEvent::Transport(event) => failures.push(format!(
            "deck {deck_index} ({}) emitted unexpected no-SYNC transport event {event:?} during {phase}",
            deck.observation.label,
        )),
        _ => {}
    }
}

/// Fold one audio event of deck `deck_index` — its seeks, underruns, failures,
/// and the resampler it plays through — into its observation and `failures`.
fn observe_audio_event(
    deck_index: usize,
    deck: &mut Deck,
    event: &AudioEvent,
    phase: &str,
    policy: EventPolicy,
    failures: &mut Vec<String>,
) {
    match event {
        AudioEvent::SeekRejected { target } => {
            deck.seek_terminal = true;
            failures.push(format!(
                "deck {deck_index} ({}) rejected seek to {:.3}s during {phase}",
                deck.observation.label,
                target.as_secs_f64()
            ));
        }
        AudioEvent::UnderrunStarted { .. } => {
            if matches!(policy, EventPolicy::MutedSeekSetup)
                && let Some(segment) = deck.seek_request_segment
            {
                if deck.muted_seek_underrun_segment.replace(segment).is_some() {
                    failures.push(format!(
                        "deck {deck_index} ({}) reported duplicate muted seek underrun during {phase}",
                        deck.observation.label,
                    ));
                }
                deck.observation.muted_seek_underruns += 1;
            } else {
                failures.push(format!(
                    "deck {deck_index} ({}) reported an underrun during {phase}",
                    deck.observation.label,
                ));
            }
        }
        AudioEvent::UnderrunEnded { .. } => {
            if deck.muted_seek_underrun_segment.is_some() {
                deck.muted_seek_underrun_segment = None;
                if !matches!(policy, EventPolicy::MutedSeekSetup) {
                    failures.push(format!(
                        "deck {deck_index} ({}) muted seek underrun recovered only after audible playback resumed during {phase}",
                        deck.observation.label,
                    ));
                }
            }
        }
        AudioEvent::TrackFailed { failure, .. } => {
            deck.seek_terminal = true;
            failures.push(format!(
                "deck {deck_index} ({}) reported track failure {failure:?} during {phase}",
                deck.observation.label,
            ));
        }
        AudioEvent::PlaybackResamplerConfigured {
            backend,
            host_sample_rate,
            source_sample_rate,
            active,
        } => deck
            .observation
            .playback_resamplers
            .push(ResamplerObservation {
                active: *active,
                backend: resampler_name(*backend),
                host_sample_rate: *host_sample_rate,
                source_sample_rate: *source_sample_rate,
            }),
        _ => {}
    }
}

pub(super) fn validate_deck(
    case: &Case,
    deck_index: usize,
    deck: &Deck,
    failures: &mut Vec<String>,
) {
    if deck.observation.decoder_changes == 0 {
        failures.push(format!(
            "{} deck {deck_index} ({}): observed no decoder changes",
            case.label, deck.observation.label,
        ));
    }
    if deck.observation.decoder_sample_rates.is_empty()
        || deck
            .observation
            .decoder_sample_rates
            .iter()
            .any(|rate| *rate != case.host_rate)
    {
        failures.push(format!(
            "{} deck {deck_index} ({}): decoder output rates {:?}, expected only {}",
            case.label,
            deck.observation.label,
            deck.observation.decoder_sample_rates,
            case.host_rate,
        ));
    }
    if deck.observation.decoder_channels.is_empty()
        || deck
            .observation
            .decoder_channels
            .iter()
            .any(|channels| *channels != case.source_channels)
    {
        failures.push(format!(
            "{} deck {deck_index} ({}): decoder channels {:?}, expected only {}",
            case.label,
            deck.observation.label,
            deck.observation.decoder_channels,
            case.source_channels,
        ));
    }
    let expected_variant = if deck.observation.hls { Some(0) } else { None };
    if deck.observation.decoder_variants.is_empty()
        || deck
            .observation
            .decoder_variants
            .iter()
            .any(|variant| *variant != expected_variant)
    {
        failures.push(format!(
            "{} deck {deck_index} ({}): decoder variants {:?}, expected only {:?}",
            case.label, deck.observation.label, deck.observation.decoder_variants, expected_variant,
        ));
    }
    let expected_active = case.host_rate != SOURCE_RATE;
    for resampler in &deck.observation.playback_resamplers {
        if resampler.host_sample_rate != case.host_rate
            || resampler.source_sample_rate != SOURCE_RATE
            || resampler.active != expected_active
        {
            failures.push(format!(
                "{} deck {deck_index} ({}): resampler {} reported source={} host={} active={}, expected source={SOURCE_RATE} host={} active={expected_active}",
                case.label,
                deck.observation.label,
                resampler.backend,
                resampler.source_sample_rate,
                resampler.host_sample_rate,
                resampler.active,
                case.host_rate,
            ));
        }
    }
}

/// Records a failure unless the session transport stands at `expected`:
/// `None` while it has rendered no block. Nothing changes the tempo of a
/// no-SYNC session, so once rendering it stays at its first revision.
pub(super) async fn record_transport_state(
    host: &OfflineHostHarness<TestPools>,
    expected: Option<TransportRevision>,
    phase: &str,
    failures: &mut Vec<String>,
) {
    let actual = match host.transport_revision().await {
        Ok(revision) => Some(revision),
        Err(PlayError::Session(SessionError::TransportNotProcessed)) => None,
        Err(error) => {
            failures.push(format!("session transport returned {error} {phase}"));
            return;
        }
    };
    if actual != expected {
        failures.push(format!(
            "session transport stands at {actual:?} {phase}, expected {expected:?} in a no-SYNC matrix",
        ));
    }
}

const fn resampler_name(kind: PlaybackResamplerKind) -> &'static str {
    match kind {
        PlaybackResamplerKind::Rubato => "rubato",
        PlaybackResamplerKind::Glide => "glide",
        PlaybackResamplerKind::None => "none",
    }
}
