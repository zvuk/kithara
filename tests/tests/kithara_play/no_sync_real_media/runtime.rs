use kithara::{
    events::{
        AudioEvent, BusEvent, DecoderEvent, DownloaderEvent, Event, EventReceiver, FileEvent,
        HlsEvent, ItemEvent, PlaybackResamplerKind, PlayerEvent, SeekLifecycleStage,
    },
    platform::{sync::Arc, tokio::sync::broadcast::error::TryRecvError},
    play::{Cmd, PlayerImpl, Reply, Resource, SessionDispatcher, SessionError},
    warp::{StretchControls, StretchKind},
};
use kithara_integration_tests::offline::OfflineSession;
use serde::Serialize;

use super::{CHANNELS, Case, SEEK_POSITION_TOLERANCE_SECS, SOURCE_RATE};
use crate::bufpool_ext::TestPools;

pub(super) struct Deck {
    pub(super) player: Arc<PlayerImpl<TestPools>>,
    pub(super) reference: Resource,
    pub(super) reference_events: EventReceiver,
    pub(super) controls: Arc<StretchControls>,
    pub(super) events: EventReceiver,
    pub(super) observation: DeckObservation,
    pub(super) seek_request_epoch: Option<u64>,
    pub(super) seek_complete_epoch: Option<u64>,
    pub(super) muted_seek_underrun_epoch: Option<u64>,
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
        loop {
            match deck.events.try_recv() {
                Ok(envelope) => match envelope.event {
                    Event::Audio(AudioEvent::SeekLifecycle {
                        stage: SeekLifecycleStage::SeekRequest,
                        seek_epoch,
                        ..
                    }) => {
                        if deck.seek_request_epoch.replace(seek_epoch).is_some() {
                            failures.push(format!(
                                "deck {deck_index} ({}) observed duplicate seek request during {phase}",
                                deck.observation.label,
                            ));
                        }
                    }
                    Event::Audio(AudioEvent::SeekComplete {
                        position,
                        seek_epoch,
                    }) => {
                        deck.observation
                            .seek_positions_secs
                            .push(position.as_secs_f64());
                        if deck.seek_request_epoch != Some(seek_epoch) {
                            failures.push(format!(
                                "deck {deck_index} ({}) completed stale seek epoch {seek_epoch} during {phase}; expected {:?}",
                                deck.observation.label, deck.seek_request_epoch,
                            ));
                            deck.seek_terminal = true;
                        } else if (position.as_secs_f64() - deck.capture_target_secs).abs()
                            > SEEK_POSITION_TOLERANCE_SECS
                        {
                            failures.push(format!(
                                "deck {deck_index} ({}) seek epoch {seek_epoch} committed at {:.9}s during {phase}, expected {:.9}s",
                                deck.observation.label,
                                position.as_secs_f64(),
                                deck.capture_target_secs,
                            ));
                            deck.seek_terminal = true;
                        } else {
                            deck.seek_complete_epoch = Some(seek_epoch);
                        }
                    }
                    Event::Audio(AudioEvent::SeekRejected { epoch, target }) => {
                        deck.seek_terminal = true;
                        failures.push(format!(
                            "deck {deck_index} ({}) rejected seek to {:.3}s during {phase}",
                            deck.observation.label,
                            target.as_secs_f64(),
                        ));
                        if deck.seek_request_epoch != Some(epoch) {
                            failures.push(format!(
                                "deck {deck_index} ({}) rejected stale seek epoch {epoch}; expected {:?}",
                                deck.observation.label, deck.seek_request_epoch,
                            ));
                        }
                    }
                    Event::Audio(AudioEvent::UnderrunStarted { seek_epoch, .. }) => {
                        if matches!(policy, EventPolicy::MutedSeekSetup)
                            && deck.seek_request_epoch == Some(seek_epoch)
                        {
                            if deck
                                .muted_seek_underrun_epoch
                                .replace(seek_epoch)
                                .is_some()
                            {
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
                    Event::Audio(AudioEvent::UnderrunEnded { seek_epoch, .. }) => {
                        if deck.muted_seek_underrun_epoch == Some(seek_epoch) {
                            deck.muted_seek_underrun_epoch = None;
                            if !matches!(policy, EventPolicy::MutedSeekSetup) {
                                failures.push(format!(
                                    "deck {deck_index} ({}) muted seek underrun recovered only after audible playback resumed during {phase}",
                                    deck.observation.label,
                                ));
                            }
                        }
                    }
                    Event::Audio(AudioEvent::TrackFailed { failure, .. }) => {
                        deck.seek_terminal = true;
                        failures.push(format!(
                            "deck {deck_index} ({}) reported track failure {failure:?} during {phase}",
                            deck.observation.label,
                        ));
                    }
                    Event::Audio(AudioEvent::PlaybackResamplerConfigured {
                        backend,
                        host_sample_rate,
                        source_sample_rate,
                        active,
                    }) => deck.observation.playback_resamplers.push(ResamplerObservation {
                        active,
                        backend: resampler_name(backend),
                        host_sample_rate,
                        source_sample_rate,
                    }),
                    Event::Decoder(DecoderEvent::DecoderChanged {
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
                    Event::Decoder(DecoderEvent::DecodeError {
                        class,
                        kind,
                        detail,
                        ..
                    }) => failures.push(format!(
                        "deck {deck_index} ({}) decode error {class:?}/{kind:?} ({detail}) during {phase}",
                        deck.observation.label,
                    )),
                    Event::Player(PlayerEvent::ItemDidPlayToEnd { item }) => {
                        deck.seek_terminal = true;
                        failures.push(format!(
                            "deck {deck_index} ({}) reached player EOF during {phase}",
                            item.track(),
                        ));
                    }
                    Event::Player(PlayerEvent::ItemDidFail { .. }) => {
                        deck.seek_terminal = true;
                        failures.push(format!(
                            "deck {deck_index} ({}) reported player track failure during {phase}",
                            deck.observation.label,
                        ));
                    }
                    Event::Bus(BusEvent::Overflow { dropped, .. }) => failures.push(format!(
                        "deck {deck_index} ({}) event bus dropped {dropped} events during {phase}",
                        deck.observation.label,
                    )),
                    Event::Hls(HlsEvent::Error { error }) => failures.push(format!(
                        "deck {deck_index} ({}) HLS error {error:?} during {phase}",
                        deck.observation.label,
                    )),
                    Event::File(FileEvent::Error { error }) => failures.push(format!(
                        "deck {deck_index} ({}) file error {error:?} during {phase}",
                        deck.observation.label,
                    )),
                    Event::Downloader(DownloaderEvent::RequestFailed { error, .. }) => {
                        failures.push(format!(
                            "deck {deck_index} ({}) downloader request failed with {error:?} during {phase}",
                            deck.observation.label,
                        ));
                    }
                    Event::Downloader(DownloaderEvent::RetryExhausted { error, .. }) => {
                        failures.push(format!(
                            "deck {deck_index} ({}) downloader exhausted retries with {error:?} during {phase}",
                            deck.observation.label,
                        ));
                    }
                    Event::Item(ItemEvent::PlaybackStalled) => failures.push(format!(
                        "deck {deck_index} ({}) playback stalled during {phase}",
                        deck.observation.label,
                    )),
                    Event::Transport(event) => failures.push(format!(
                        "deck {deck_index} ({}) emitted unexpected no-SYNC transport event {event:?} during {phase}",
                        deck.observation.label,
                    )),
                    _ => {}
                },
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
            .any(|channels| *channels != CHANNELS)
    {
        failures.push(format!(
            "{} deck {deck_index} ({}): decoder channels {:?}, expected only {CHANNELS}",
            case.label, deck.observation.label, deck.observation.decoder_channels,
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

pub(super) fn record_control_state(
    case: &Case,
    deck_index: usize,
    deck: &Deck,
    phase: &str,
    failures: &mut Vec<String>,
) {
    if deck.controls.speed() != 1.0
        || deck.controls.region_plan().is_some()
        || deck.controls.backend() != StretchKind::Signalsmith
        || !deck.controls.keylock()
    {
        failures.push(format!(
            "{} deck {deck_index} ({}): invalid no-SYNC controls {phase} (speed={}, plan={}, backend={:?}, keylock={})",
            case.label,
            deck.observation.label,
            deck.controls.speed(),
            deck.controls.region_plan().is_some(),
            deck.controls.backend(),
            deck.controls.keylock(),
        ));
    }
}

pub(super) fn record_transport_state(
    session: &OfflineSession,
    phase: &str,
    failures: &mut Vec<String>,
) {
    match session.exec(Cmd::QuerySessionTransport) {
        Ok(Reply::Err(SessionError::TransportNotProcessed)) => {}
        Ok(Reply::Err(error)) => failures.push(format!(
            "session transport returned {error} {phase}, expected unconfigured",
        )),
        Ok(_) => failures.push(format!(
            "session transport was configured {phase}, but this is a no-SYNC matrix",
        )),
        Err(error) => failures.push(format!("session transport query failed {phase}: {error}")),
    }
}

const fn resampler_name(kind: PlaybackResamplerKind) -> &'static str {
    match kind {
        PlaybackResamplerKind::Rubato => "rubato",
        PlaybackResamplerKind::Glide => "glide",
        PlaybackResamplerKind::None => "none",
        _ => "unknown",
    }
}
