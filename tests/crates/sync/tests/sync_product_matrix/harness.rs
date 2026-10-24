use kithara::{
    audio::{
        AudioDecoderConfig, DecoderBackend as EventDecoderBackend, DecoderChangeCause, DecoderEvent,
    },
    decode::DecoderBackend,
    events::{EventBus, TrackId},
    play::Settled,
    signal::SegmentId,
};
use kithara_command::Seq;
use kithara_integration_tests::offline::linked::{LinkReceiptOrigin, LinkSpeed};
use kithara_render::bridge::{Slot, SlotSnapshot};
use num_traits::ToPrimitive;

use super::{cases::*, imports::*, providers::*};

#[derive(Clone, Copy, Debug)]
pub(crate) enum SyncRequest {
    On,
    Off,
    Align,
}

enum CaptureMode {
    SourceBoundaries,
    Fixed,
    Paced,
}

pub(crate) struct ProductHarness {
    pub(crate) decks: Vec<LinkedQueueHandle<TestPools>>,
    pub(crate) failures: Vec<String>,
    block_frames: usize,
    pub(crate) host: OfflineHostHarness<TestPools, LinkedOwner<TestPools>>,
    /// The limited mix before the metronome: what the oracles read.
    master: TapProbe,
    /// Keeps real renderer probes scoped to this harness.
    _trace: usdt_trace::Scope,
    output_frames: u64,
    provider: Provider,
    worker: PlayWorker<TestPools>,
    trajectory: TempoTrajectory,
    operation_grid: Option<kithara::warp::BeatGridSnapshot>,
    capture_marks: Vec<(SessionFrame, TrackId, SlotSnapshot)>,
    host_grids: Vec<(SessionFrame, kithara::warp::BeatGridSnapshot)>,
    decoded: Vec<Arc<Mutex<super::control::DecodedRecorder>>>,
    source_uris: Vec<String>,
    /// The second each deck's start opens it at, before its stagger.
    cues: Vec<f64>,
    decoder_backends: Vec<EventDecoderBackend>,
    decoder_rates: Vec<Option<(u32, u32)>>,
    /// Every rendered block as `graph_out` plays it, metronome included.
    #[cfg(not(target_os = "android"))]
    tap: kithara_integration_tests::audio_artifact::AudioArtifactTap,
    #[cfg(not(target_os = "android"))]
    master_tap: kithara_integration_tests::audio_artifact::AudioArtifactTap,
    #[cfg(not(target_os = "android"))]
    metronome_delta_peak: f32,
}

#[cfg(not(target_os = "android"))]
fn verify_speed_references(observation: &LinkObservation) {
    for (index, entry) in observation.timeline.iter().enumerate() {
        if entry["event"].as_str() != Some("command") {
            continue;
        }
        for operation in ["SetSpeed", "Align"] {
            let Some(input) = entry["details"][operation].get("speed") else {
                continue;
            };
            let admission = &observation.timeline[index + 1];
            assert_eq!(admission["event"].as_str(), Some("admission"));
            let result = admission["details"].as_str().expect("command admission");
            if let Some(input) = input["input"].as_u64() {
                let speed = observation
                    .speeds
                    .get(usize::try_from(input).expect("speed input index"))
                    .expect("admitted command retains its exact curve");
                assert_eq!(entry["item"].as_u64(), Some(u64::from(speed.item)));
                assert_eq!(result, format!("Ok(Some({:?}))", speed.seq));
            } else {
                assert!(
                    input.is_string(),
                    "unadmitted command retains its full curve"
                );
                assert!(
                    !result.starts_with("Ok(Some("),
                    "admitted command must reference its retained input"
                );
            }
        }
    }
}

#[cfg(not(target_os = "android"))]
fn curve_evidence_key(curve: &kithara::sync::SpeedCurve) -> Vec<u64> {
    use kithara::sync::SpeedCurve;
    let approach = |value: kithara::signal::ExponentialApproach| {
        [
            value.initial().to_bits(),
            value.target().to_bits(),
            value.time_constant().to_bits(),
        ]
    };
    match curve {
        SpeedCurve::Constant(speed) => vec![0, u64::from(speed.to_bits())],
        SpeedCurve::Approach(value) => {
            let mut key = Vec::with_capacity(4);
            key.push(1);
            key.extend(approach(*value));
            key
        }
        SpeedCurve::ApproachSteps(parts) => {
            let mut key = Vec::with_capacity(1 + parts.len() * 4);
            key.push(2);
            for (frame, value) in parts.iter() {
                key.push(*frame);
                key.extend(approach(*value));
            }
            key
        }
        SpeedCurve::Ramp { to, frames } => vec![3, u64::from(to.to_bits()), frames.get()],
        SpeedCurve::Steps(parts) => {
            let mut key = Vec::with_capacity(1 + parts.len() * 2);
            key.push(4);
            for (frame, value) in parts.iter() {
                key.extend([*frame, u64::from(value.to_bits())]);
            }
            key
        }
    }
}

#[cfg(not(target_os = "android"))]
fn curve_evidence(
    curves: &mut std::collections::HashMap<Vec<u64>, usize>,
    curve: &kithara::sync::SpeedCurve,
) -> serde_json::Value {
    let key = curve_evidence_key(curve);
    let next = curves.len();
    match curves.entry(key) {
        std::collections::hash_map::Entry::Occupied(entry) => {
            serde_json::json!({"curve": entry.get()})
        }
        std::collections::hash_map::Entry::Vacant(entry) => {
            entry.insert(next);
            serde_json::json!({"curve": next, "definition": format!("{curve:?}")})
        }
    }
}

#[cfg(not(target_os = "android"))]
#[kithara::test]
fn curve_evidence_references_preserve_every_exact_definition() {
    use std::{collections::HashMap, num::NonZeroU64};

    use kithara::{signal::ExponentialApproach, sync::SpeedCurve};

    let approach = ExponentialApproach::new(1.0, 1.5, 240.0).expect("finite approach");
    let signed_zero = ExponentialApproach::new(1.0, 1.5, -0.0).expect("signed zero approach");
    let curves = [
        SpeedCurve::Constant(0.0),
        SpeedCurve::Constant(-0.0),
        SpeedCurve::Constant(1.0),
        SpeedCurve::Approach(approach),
        SpeedCurve::Approach(signed_zero),
        SpeedCurve::ApproachSteps(vec![(0, approach), (96, signed_zero)].into()),
        SpeedCurve::ApproachSteps(vec![(0, approach), (97, signed_zero)].into()),
        SpeedCurve::Ramp {
            to: 1.0,
            frames: NonZeroU64::new(96).expect("nonzero frames"),
        },
        SpeedCurve::Ramp {
            to: 1.0,
            frames: NonZeroU64::new(97).expect("nonzero frames"),
        },
        SpeedCurve::Steps(vec![(0, 1.0), (96, 1.5)].into()),
        SpeedCurve::Steps(vec![(0, 1.0), (96, 1.25)].into()),
    ];
    let mut keys = HashMap::new();
    let mut definitions = HashMap::new();
    for (index, curve) in curves.iter().cycle().take(curves.len() * 3).enumerate() {
        let evidence = curve_evidence(&mut keys, curve);
        let id = evidence["curve"].as_u64().expect("curve reference");
        if let Some(definition) = evidence.get("definition") {
            assert!(index < curves.len(), "each definition is emitted once");
            assert!(
                definitions
                    .insert(
                        id,
                        definition
                            .as_str()
                            .expect("full Debug definition")
                            .to_owned()
                    )
                    .is_none()
            );
        }
        assert_eq!(definitions[&id], format!("{curve:?}"));
    }
    assert_eq!(
        keys.len(),
        curves.len(),
        "every distinct bit pattern stays distinct"
    );
    assert_eq!(definitions.len(), curves.len());
}

fn native_start_at(
    observation: &LinkObservation,
    item: TrackId,
    seq: Seq,
    slot: Slot,
) -> Option<i64> {
    observation.timeline.iter().rev().find_map(|entry| {
        if entry["event"].as_str() != Some("deck receipt")
            || entry["item"].as_u64() != Some(u64::from(item))
        {
            return None;
        }
        let details = &entry["details"];
        if details["seq"].as_u64() != Some(seq.get())
            || !details["parts"]
                .as_array()?
                .iter()
                .any(|part| part["Start"]["slot"].as_u64() == Some(u64::from(slot.get())))
        {
            return None;
        }
        details["applied_frame"].as_i64()
    })
}

fn stable_capture_at(
    observation: &LinkObservation,
    item: TrackId,
    start: i64,
    declick: i64,
) -> Option<i64> {
    let stable = start.checked_add(declick)?;
    let Some(speed) = observation
        .speeds
        .iter()
        .rev()
        .find(|speed| speed.item == item)
    else {
        return Some(stable);
    };
    let applied = observation
        .placement_receipts
        .iter()
        .find_map(|(owner, origin, receipt)| match receipt {
            Settled::Applied { seq, at }
                if *owner == item && *origin == LinkReceiptOrigin::Lane && *seq == speed.seq =>
            {
                Some(i64::from(*at))
            }
            _ => None,
        })?;
    let completed = if let Some((_, landing)) = speed.landing {
        let landing = i64::from(landing);
        let fade = landing.checked_sub(applied).filter(|frames| *frames >= 0)?;
        landing.checked_add(fade)?
    } else {
        applied
    };
    Some(stable.max(completed))
}

fn segment_custody(
    observation: &LinkObservation,
    item: TrackId,
    slot: Slot,
    segment: SegmentId,
) -> Option<(i64, serde_json::Value)> {
    let &(_, load) = observation.loads.iter().find(|(owner, _)| *owner == item)?;
    let (_, prepared) = observation
        .source_clocks
        .iter()
        .find(|(owner, _)| *owner == item)?;
    if segment == prepared.segment {
        return observation
            .placement_receipts
            .iter()
            .find_map(|(owner, origin, receipt)| {
                if *owner != item || *origin != LinkReceiptOrigin::Deck {
                    return None;
                }
                match receipt {
                    Settled::Applied { seq, at } if *seq == load => Some((
                        i64::from(*at),
                        serde_json::json!({
                            "operation": "Load", "load_seq": load.get(),
                            "applied_frame": i64::from(*at), "segment": segment.get(),
                            "slot": slot.get(),
                        }),
                    )),
                    _ => None,
                }
            });
    }
    observation.timeline.iter().rev().find_map(|entry| {
        if entry["event"].as_str() != Some("deck receipt")
            || entry["item"].as_u64() != Some(u64::from(item))
        {
            return None;
        }
        let details = &entry["details"];
        let applied_at = details["applied_frame"].as_i64()?;
        details["parts"]
            .as_array()?
            .iter()
            .any(|part| {
                part["Adopt"]["slot"].as_u64() == Some(u64::from(slot.get()))
                    && part["Adopt"]["segment"].as_u64() == Some(segment.get())
            })
            .then(|| (applied_at, details.clone()))
    })
}

fn linked_mixer() -> kithara::play::DeckMixerConfig {
    kithara::play::DeckMixerConfig::builder()
        .eq_bands(0)
        .mix(
            kithara::play::DeckMixSettings::builder()
                .level(super::control::CONTROL_GAIN)
                .build(),
        )
        .build()
}

fn linked_queue(
    case: SyncCase,
    worker: &PlayWorker<TestPools>,
    trajectory: &TempoTrajectory,
    observation: &Arc<Mutex<LinkObservation>>,
    block_frames: usize,
) -> kithara_integration_tests::offline::linked::LinkedQueue<TestPools> {
    Queue::new(
        QueueConfig::with_factory(LinkedFactory::new(
            LinkedConfig::builder()
                .inner(ObservedFactory::new(observation.clone()))
                .settings(LinkConfig::default())
                .trajectory(trajectory.clone())
                .build(),
        ))
        .mixer(linked_mixer())
        .should_autoplay(false)
        .prep(
            ResourcePrep::builder()
                .worker(worker.clone())
                .block_on_underrun(true)
                .warp(
                    kithara::warp::WarpConfig::builder()
                        .render_quantum_frames(
                            NonZeroUsize::new(block_frames.min(128))
                                .expect("fixture render quantum"),
                        )
                        .build(),
                )
                .maybe_response_budget_frames(case.response_budget)
                .build(),
        )
        .store(memory_asset_store())
        .build(),
    )
}

/// Which decks a harness run hears.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Audible {
    /// One deck solo; every other deck is muted.
    Deck(usize),
    /// Every deck.
    Mix,
}

impl Audible {
    const fn hears(self, deck: usize) -> bool {
        match self {
            Self::Deck(solo) => solo == deck,
            Self::Mix => true,
        }
    }

    #[cfg(not(target_os = "android"))]
    fn label(self) -> String {
        match self {
            Self::Deck(deck) => format!("deck-{deck}"),
            Self::Mix => "mix".to_owned(),
        }
    }
}

/// Listening artifacts of each desktop harness run with the Host metronome.
/// They encode through the app's recording stack, which no device build carries.
#[cfg(not(target_os = "android"))]
mod artifact {
    use kithara_integration_tests::audio_artifact::{AudioArtifactTap, artifact_label};

    use super::{Audible, CHANNELS, Provider, SyncCase};

    /// The artifact of one harness run, named by the running test, the case and
    /// the decks it hears.
    pub(crate) fn open_tap(
        case: SyncCase,
        provider: Provider,
        audible: Audible,
        stream: &str,
    ) -> AudioArtifactTap {
        let default_root = std::env::current_exe()
            .unwrap_or_else(|error| panic!("{}: locate the test executable: {error}", case.id))
            .with_file_name("audio-artifacts");
        let mut tap = AudioArtifactTap::from_env_or(
            &default_root,
            &format!(
                "{}-{}-{}-{stream}",
                artifact_label(),
                case.id,
                audible.label()
            ),
            case.sample_rate,
            CHANNELS,
        )
        .unwrap_or_else(|error| panic!("{}: open the listening artifact: {error}", case.id));
        tap.evidence(
            "provider",
            serde_json::Value::String(format!("{provider:?}")),
        );
        tap
    }
}

impl ProductHarness {
    /// A harness whose decks seek to `start` when the case seeks.
    pub(crate) async fn new(
        case: SyncCase,
        prepared: &PreparedSources,
        start: Start,
        audible: Audible,
    ) -> Self {
        Self::build(case, prepared, start, audible, BLOCK_FRAMES, None).await
    }

    pub(crate) async fn new_for_block(
        case: SyncCase,
        prepared: &PreparedSources,
        start: Start,
        audible: Audible,
        block_frames: usize,
    ) -> Self {
        Self::build(case, prepared, start, audible, block_frames, None).await
    }

    pub(crate) async fn new_with_decoder(
        case: SyncCase,
        prepared: &PreparedSources,
        start: Start,
        audible: Audible,
        decoder: DecoderBackend,
    ) -> Self {
        Self::build(case, prepared, start, audible, BLOCK_FRAMES, Some(decoder)).await
    }

    pub(crate) fn assert_decoder_backend(&self, expected: EventDecoderBackend) {
        assert_eq!(self.decoder_backends.len(), self.decks.len());
        assert!(
            self.decoder_backends
                .iter()
                .all(|backend| *backend == expected),
            "requested decoder must own every source: {:?}",
            self.decoder_backends
        );
    }

    #[kithara_test_utils::kithara::hang_watchdog(timeout = Duration::from_secs(5), ctx = String)]
    async fn build(
        case: SyncCase,
        prepared: &PreparedSources,
        start: Start,
        audible: Audible,
        block_frames: usize,
        decoder: Option<DecoderBackend>,
    ) -> Self {
        let provider = prepared.0;
        let sources: Vec<_> = prepared
            .2
            .iter()
            .cycle()
            .take(case.decks)
            .cloned()
            .collect();
        let source_uris = sources.clone();
        let pools = pools();
        let worker = PlayWorker::new(
            PlayWorkerConfig::builder(pools.clone())
                .maybe_capacity(case.capacity)
                .build(),
        );
        let sample_rate = NonZeroU32::new(case.sample_rate).expect("fixture sample rate");
        let render_block_frames = NonZeroU32::new(
            u32::try_from(block_frames).expect("offline render block count fits u32"),
        )
        .expect("offline render block count is non-zero");
        let session = HostConfig::offline(pools)
            .max_block_frames(render_block_frames)
            .settings(
                HostSettings::builder()
                    .sample_rate(sample_rate)
                    .metronome(
                        MetronomeConfig::builder()
                            .level(METRONOME_DUCK)
                            .duck(METRONOME_DUCK)
                            .build(),
                    )
                    .build(),
            )
            .build();
        let trace = usdt_trace::scope();
        let trajectory = TempoTrajectory::new(
            TempoStep {
                frame: SessionFrame::new(0),
                beat: SessionBeat::default(),
                tempo: Tempo::new(START_BPM).expect("initial tempo"),
            },
            std::num::NonZeroU16::new(4).expect("fixture meter"),
            sample_rate,
        );
        let host_axis = trajectory.clone();
        let host = OfflineHostHarness::layered(session, None, move |core| {
            LinkedHost::new(
                LinkedHostConfig::builder()
                    .inner(core)
                    .trajectory(host_axis)
                    .build(),
            )
        })
        .await
        .unwrap_or_else(|error| panic!("{}: create offline Host: {error}", case.id));
        host.with(|host| host.metronome().set_enabled(true))
            .await
            .unwrap_or_else(|error| panic!("{}: metronome: {error}", case.id));
        hang_tick!(format!(
            "{}: publishing the offline clock and stream shape",
            case.id
        ));
        let _ = host.render(block_frames).await;
        hang_reset!();
        let output_frames = host.position();
        let master = host
            .attach_tap(
                Tap::Master,
                case.sample_rate as usize * usize::from(CHANNELS) * MASTER_TAP_SECONDS,
            )
            .await
            .unwrap_or_else(|error| panic!("{}: master tap: {error}", case.id));
        let mut decks = Vec::with_capacity(sources.len());
        let mut ids = Vec::with_capacity(sources.len());
        let mut decoder_receivers = Vec::new();
        let mut decoded = Vec::with_capacity(sources.len());
        for (index, source) in sources.into_iter().enumerate() {
            hang_tick!(format!("{}: creating deck {index}", case.id));
            // Ruling: PlayerImpl-backed Host deck → Queue<LinkedFactory<ObservedFactory>> on LinkedHost — spec §4.4–§4.6, §11.2.
            let observation = Arc::new(Mutex::new(LinkObservation::default()));
            let queue = linked_queue(case, &worker, &trajectory, &observation, block_frames);
            let deck = host
                .insert_linked(queue, observation)
                .await
                .unwrap_or_else(|error| panic!("{}: insert deck {index}: {error}", case.id));
            let config = ResourceConfig::for_src(
                ResourceSrc::parse(&source)
                    .unwrap_or_else(|error| panic!("{}: parse source {source}: {error}", case.id)),
            )
            .downloader(prepared.1.modeled_downloader())
            .store(memory_asset_store())
            .initial_abr_mode(AbrMode::manual(0))
            .discriminator(format!("{}-{provider:?}-{index}", case.id))
            .maybe_beat_grid(case.gridded.then(|| provider.beat_grid(index)));
            let events = EventBus::default();
            decoder_receivers.push(events.subscribe::<DecoderEvent>());
            let config = config.events(events);
            let config = if let Some(backend) = decoder {
                config
                    .decoder(AudioDecoderConfig::builder().backend(backend).build())
                    .build()
            } else {
                config.build()
            };
            let control = deck.control().clone();
            let muted = !audible.hears(index);
            let (reference, observer) = super::control::DecodedRecorder::open();
            let id = host
                .run(move || {
                    control.set_muted(muted).expect("deck mute");
                    let item = control.append(TrackSource::Config(Box::new(config)))?;
                    control.attach_observer(item, observer);
                    Ok::<_, kithara::queue::QueueError>(item)
                })
                .await
                .unwrap_or_else(|error| panic!("{}: append deck {index}: {error}", case.id));
            decks.push(deck);
            ids.push(id);
            decoded.push(reference);
            hang_reset!();
        }
        let mut harness = Self {
            decks,
            failures: Vec::new(),
            block_frames,
            host,
            master,
            output_frames,
            provider,
            worker,
            trajectory,
            operation_grid: None,
            capture_marks: Vec::new(),
            host_grids: Vec::new(),
            decoded,
            source_uris,
            decoder_backends: Vec::new(),
            decoder_rates: Vec::new(),
            cues: (0..case.decks)
                .map(|deck| provider.start_seconds(deck, start))
                .collect(),
            #[cfg(not(target_os = "android"))]
            tap: artifact::open_tap(case, provider, audible, "output"),
            #[cfg(not(target_os = "android"))]
            master_tap: artifact::open_tap(case, provider, audible, "master"),
            #[cfg(not(target_os = "android"))]
            metronome_delta_peak: 0.0,
            _trace: trace,
        };
        let _ = harness.render(case, block_frames).await;
        hang_reset!();
        harness.wait_loaded(case, &ids).await;
        for (deck, mut receiver) in decoder_receivers.into_iter().enumerate() {
            let event = receiver
                .try_recv()
                .expect("the loaded source publishes its initial decoder")
                .event;
            if decoder.is_some() {
                eprintln!("DECODER_RECEIPT deck={deck} requested={decoder:?} event={event:?}");
            }
            let DecoderEvent::DecoderChanged {
                backend,
                cause: DecoderChangeCause::Initial,
                ..
            } = event
            else {
                panic!(
                    "the first source decoder event must identify the initial generation: {event:?}"
                );
            };
            harness.decoder_backends.push(backend);
            let rates =
                std::iter::from_fn(|| receiver.try_recv().ok()).find_map(|event| {
                    match event.event {
                        DecoderEvent::ResamplerConfigured {
                            input_rate,
                            output_rate,
                            ..
                        } => Some((input_rate, output_rate)),
                        _ => None,
                    }
                });
            harness.decoder_rates.push(rates);
        }
        harness.deliver_grids(case).await;
        harness.set_tempo(case, case.start_bpm(), true).await;
        harness.warm_up_transport(case).await;
        if !case.paused {
            harness.start_staggered(case).await;
        }
        harness
    }

    #[kithara_test_utils::kithara::hang_watchdog(timeout = Duration::from_secs(5), ctx = String)]
    pub(crate) async fn add_seated_deck(&mut self, case: SyncCase) -> usize {
        let index = self.decks.len();
        hang_tick!(format!(
            "{}: inserting capacity contender deck {index}",
            case.id
        ));
        let observation = Arc::new(Mutex::new(LinkObservation::default()));
        let queue = linked_queue(
            case,
            &self.worker,
            &self.trajectory,
            &observation,
            self.block_frames,
        );
        let deck = self
            .host
            .insert_linked(queue, observation)
            .await
            .unwrap_or_else(|error| panic!("{}: insert capacity contender: {error}", case.id));
        hang_reset!();
        self.decks.push(deck);
        index
    }

    /// Render a fixed span so PCM comparisons start on the same frame, then
    /// require a loaded track, its dispatcher receipt and an applied renderer receipt.
    #[kithara_test_utils::kithara::hang_watchdog(timeout = Duration::from_secs(5), ctx = String)]
    async fn warm_up_transport(&mut self, case: SyncCase) {
        let frames = u64::try_from(WARM_UP_BLOCKS * self.block_frames)
            .expect("fixture transport warm-up span");
        while self.output_frames < frames {
            hang_tick!(format!(
                "{}: warming frame {} to {frames}",
                case.id, self.output_frames
            ));
            let _ = self.render(case, self.block_frames).await;
            hang_reset!();
        }
        // Ruling: processed TransportRevision probe → real load Settled and published track snapshot — spec §11.2, F belongs to LinkedHost.
        for deck in &self.decks {
            let item = deck.current().expect("selected item").id;
            let snapshot = deck
                .track_snapshot(item)
                .expect("Host publishes its loaded track");
            assert!(
                snapshot.track.attached,
                "{}: renderer attached the loaded track",
                case.id
            );
            deck.observe(|observation| {
                let load = observation
                    .loads
                    .iter()
                    .find_map(|&(owner, seq)| (owner == item).then_some(seq))
                    .expect("real Load sequence");
                assert!(
                    observation.dispatcher.contains(&(
                        load,
                        kithara_integration_tests::offline::linked::LinkVerdict::Applied
                    )),
                    "{}: dispatcher acknowledged that Load",
                    case.id
                );
                assert!(
                    observation
                        .settled
                        .iter()
                        .any(|(owner, receipt)| *owner == item
                            && matches!(receipt, Settled::Applied { .. })),
                    "{}: renderer acknowledged that track",
                    case.id
                );
            });
        }
    }

    #[kithara_test_utils::kithara::hang_watchdog(timeout = Duration::from_secs(5), ctx = String)]
    async fn wait_loaded(&mut self, case: SyncCase, ids: &[TrackId]) {
        let deadline = Instant::now() + LOAD_TIMEOUT;
        let mut completed = 0;
        loop {
            hang_tick!(format!(
                "{}: awaiting loads {ids:?}, {completed} ready",
                case.id
            ));
            self.tick_all(case).await;
            let mut loaded = true;
            let mut ready = 0;
            for (index, (deck, id)) in self.decks.iter().zip(ids).enumerate() {
                match deck.track(*id).map(|track| track.status) {
                    Some(TrackStatus::Loaded | TrackStatus::Consumed) => ready += 1,
                    Some(TrackStatus::Failed(error)) => {
                        panic!("{}: deck {index} failed to load: {error}", case.id)
                    }
                    Some(_) | None => loaded = false,
                }
                loaded &= deck.current().is_some_and(|track| track.id == *id);
            }
            if loaded {
                hang_reset!();
                return;
            }
            if ready > completed {
                completed = ready;
                hang_reset!();
            }
            assert!(
                Instant::now() < deadline,
                "{}: deck load timed out",
                case.id
            );
            if ready == ids.len() {
                let _ = self.render(case, self.block_frames).await;
            } else {
                let _real_io = kithara::platform::flash::real_io();
                time::sleep(Duration::from_millis(1).min(__hang_detector.remaining())).await;
            }
        }
    }

    /// Ticks every deck from the host owner thread, as the app update loop
    /// would.
    async fn tick_all(&self, case: SyncCase) -> kithara::warp::BeatGridSnapshot {
        use kithara::warp::BeatGrid;
        let controls: Vec<_> = self
            .decks
            .iter()
            .map(|deck| deck.control().clone())
            .collect();
        self.host
            .with(move |host| {
                for (index, control) in controls.iter().enumerate() {
                    control
                        .tick()
                        .unwrap_or_else(|error| panic!("{}: tick deck {index}: {error}", case.id));
                }
                host.snapshot()
            })
            .await
    }

    #[kithara_test_utils::kithara::hang_watchdog(timeout = Duration::from_secs(5), ctx = String)]
    pub(crate) async fn render(&mut self, case: SyncCase, frames: usize) -> Vec<f32> {
        hang_tick!(format!(
            "{}: rendering {frames} frames from {}",
            case.id, self.output_frames
        ));
        let grid = self.tick_all(case).await;
        let start_frame = SessionFrame::new(
            i64::try_from(self.output_frames).expect("observed Host frame fits its session axis"),
        );
        self.host_grids.push((start_frame, grid));
        let start = self.output_frames;
        let end = start
            .checked_add(u64::try_from(frames).expect("render frame count fits u64"))
            .expect("offline render timeline fits u64");
        let samples = self.host.render(frames).await;
        assert_eq!(self.host.position(), end);
        hang_reset!();
        let grid = self.tick_all(case).await;
        let end_frame = SessionFrame::new(
            i64::try_from(end).expect("observed Host frame fits its session axis"),
        );
        self.host_grids.push((end_frame, grid));
        self.output_frames = end;
        #[cfg(not(target_os = "android"))]
        self.tap.push(&samples);
        let master = self.master.drain();
        assert_eq!(self.master.drops(), 0, "the master tap keeps every frame");
        assert_eq!(
            master.len(),
            frames * usize::from(CHANNELS),
            "the master tap sees every rendered frame"
        );
        #[cfg(not(target_os = "android"))]
        {
            self.master_tap.push(&master);
            self.metronome_delta_peak = samples
                .iter()
                .zip(&master)
                .map(|(output, master)| (output - master).abs())
                .fold(self.metronome_delta_peak, f32::max);
            self.tap.evidence(
                "metronome_output_delta_peak",
                serde_json::json!(self.metronome_delta_peak),
            );
        }
        master
    }

    /// Stamps a control marker on the listening artifact.
    #[cfg(not(target_os = "android"))]
    pub(crate) fn mark(&mut self, label: &str) {
        self.tap.mark(label);
        self.master_tap.mark(label);
    }

    #[kithara_test_utils::kithara::hang_watchdog(timeout = Duration::from_secs(5), ctx = String)]
    pub(crate) async fn settle(&mut self, case: SyncCase, blocks: usize) {
        for _ in 0..blocks {
            hang_tick!(format!(
                "{}: settling at frame {}",
                case.id, self.output_frames
            ));
            let _ = self.render(case, self.block_frames).await;
            hang_reset!();
        }
    }

    pub(crate) async fn play_all(&self) {
        let controls: Vec<_> = self
            .decks
            .iter()
            .map(|deck| deck.control().clone())
            .collect();
        self.host
            .run(move || {
                for control in &controls {
                    control.play();
                }
            })
            .await;
    }

    #[kithara_test_utils::kithara::hang_watchdog(timeout = Duration::from_secs(5), ctx = String)]
    async fn start_staggered(&mut self, case: SyncCase) {
        let stagger_frames: usize = (f64::from(case.sample_rate) * 3.0 / 8.0 * 60.0
            / case.start_bpm())
        .round()
        .as_();
        for index in 0..self.decks.len() {
            hang_tick!(format!("start_staggered: deck {index}"));
            let control = self.decks[index].control().clone();
            self.host.run(move || control.play()).await;
            if index + 1 < self.decks.len() {
                let _ = self.render(case, stagger_frames).await;
            }
            hang_reset!();
        }
        self.settle(case, 2).await;
    }

    delegate::delegate! {
        to self.provider {
            #[call(beat_grid)]
            pub(crate) fn provider_grid(&self, deck: usize) -> ArtifactSource<BeatGridModel>;
            /// The second deck `deck`'s track opens at for `start`.
            pub(crate) fn start_seconds(&self, deck: usize, start: Start) -> f64;
        }
    }

    #[kithara_test_utils::kithara::hang_watchdog(timeout = Duration::from_secs(5), ctx = String)]
    pub(crate) async fn seek_staggered(&mut self, case: SyncCase) {
        let stagger_seconds = 3.0 / 8.0 * 60.0 / case.start_bpm();
        let mut requests = Vec::with_capacity(self.decks.len());
        for (index, deck) in self.decks.iter().enumerate() {
            hang_tick!(format!("{}: seeking deck {index}", case.id));
            let stagger: f64 = index.as_();
            let seconds = stagger.mul_add(stagger_seconds, self.cues[index]);
            #[cfg(not(target_os = "android"))]
            {
                self.tap.mark(&format!("seek deck {index} to {seconds:.9}"));
                self.master_tap
                    .mark(&format!("seek deck {index} to {seconds:.9}"));
            }
            let item = deck.current().expect("selected seek item").id;
            let before = deck.track_snapshot(item).expect("seek track").track.mark;
            let (seeks, placements) =
                deck.observe(|observation| (observation.seeks.len(), observation.placements.len()));
            requests.push((
                item,
                seeks,
                placements,
                before.map(|mark| mark.lane.segment),
            ));
            let control = deck.control().clone();
            self.host
                .run(move || control.seek(seconds))
                .await
                .unwrap_or_else(|error| panic!("{}: seek deck {index}: {error}", case.id));
            hang_reset!();
        }
        self.wait_seeks(case, &requests).await;
    }

    #[kithara_test_utils::kithara::hang_watchdog(timeout = Duration::from_secs(5), ctx = String)]
    async fn wait_seeks(
        &mut self,
        case: SyncCase,
        requests: &[(TrackId, usize, usize, Option<SegmentId>)],
    ) {
        let deadline = Instant::now() + LOAD_TIMEOUT;
        let rate = NonZeroU32::new(case.sample_rate).expect("seek sample rate");
        let declick = i64::try_from(linked_mixer().declick_frames(rate).get())
            .expect("declick fits the session axis");
        loop {
            hang_tick!(format!(
                "{}: waiting for applied Seek at {}",
                case.id, self.output_frames
            ));
            let ready = self.decks.iter().zip(requests).all(|(deck, &(item, seeks, placements, before))| {
                if deck.current().map(|entry| entry.id) != Some(item) {
                    return false;
                }
                let Some(snapshot) = deck.track_snapshot(item) else {
                    return false;
                };
                if snapshot.track.pending_lane {
                    return false;
                }
                deck.observe(|observation| {
                    let consumed = observation.consumed.iter().find(|(owner, _)| *owner == item)
                        .map(|(_, slot)| slot);
                    let sounding = consumed.and_then(|slot| {
                        let mark = slot.mark?;
                        let source = slot.source?;
                        (slot.state == kithara_render::bridge::SlotState::Playing
                            && snapshot.track.mark == Some(mark)
                            && source.segment == mark.lane.segment
                            && matches!(snapshot.track.status, kithara::play::TrackStatus::Playing { .. }))
                            .then_some(mark)
                    });
                    if let Some(placement) = observation.placements[placements..].iter()
                        .find(|placement| placement.item == item)
                    {
                        return observation.placement_applied.contains(&(item, placement.seq, true))
                            && sounding.is_some_and(|mark| {
                                placement.before.mark.is_some_and(|before| before.lane.segment == mark.lane.segment)
                                    && i64::from(mark.session) >= i64::from(placement.at) + declick
                            });
                    }
                    let Some(&(_, origin, seq)) = observation.seeks[seeks..].iter()
                        .find(|(owner, _, _)| *owner == item) else {
                            return false;
                        };
                    let applied = match origin {
                        LinkReceiptOrigin::Lane => observation.seek_applied.contains(&(item, seq, true)),
                        LinkReceiptOrigin::Deck => observation.placement_receipts.iter().any(|(owner, origin, receipt)| {
                            *owner == item && *origin == LinkReceiptOrigin::Deck
                                && matches!(receipt, Settled::Applied { seq: answered, .. } if *answered == seq)
                        }),
                    };
                    applied && (!matches!(snapshot.track.status, kithara::play::TrackStatus::Playing { .. })
                        || sounding.is_some_and(|mark| Some(mark.lane.segment) != before))
                })
            });
            if ready {
                #[cfg(not(target_os = "android"))]
                self.mark("seeks_applied");
                hang_reset!();
                return;
            }
            assert!(
                Instant::now() < deadline,
                "{}: Seek did not complete",
                case.id
            );
            let _ = self.render(case, self.block_frames.min(128)).await;
            hang_reset!();
        }
    }

    pub(crate) async fn set_tempo(&mut self, case: SyncCase, bpm: f64, required: bool) {
        #[cfg(not(target_os = "android"))]
        self.mark(&format!("tempo {bpm:.9}"));
        let tempo = Tempo::new(bpm).expect("fixture tempo");
        match self.host.with(move |host| host.set_tempo(tempo)).await {
            Ok(()) => {}
            Err(error) if !required => self.record_tempo_failure(format!(
                "tempo request {bpm:.6} BPM could not reach Host: {error}"
            )),
            Err(error) => panic!("{}: set initial tempo: {error}", case.id),
        }
    }

    fn record_tempo_failure(&mut self, failure: String) {
        if !self
            .failures
            .iter()
            .any(|failure| failure.starts_with("tempo request"))
        {
            self.failures.push(failure);
        }
    }

    #[kithara_test_utils::kithara::hang_watchdog(timeout = Duration::from_secs(5), ctx = String)]
    async fn deliver_grids(&self, case: SyncCase) {
        // Ruling: SyncOperation source/load grid topology → item/load-scoped GridAnswer — spec §4.8, §11.2.
        for (index, deck) in self.decks.iter().enumerate() {
            hang_tick!(format!("deliver_grids: deck {index}"));
            let item = deck.current().expect("selected item").id;
            let load = deck
                .observe(|observation| {
                    observation
                        .loads
                        .iter()
                        .rev()
                        .find(|(loaded, _)| *loaded == item)
                        .map(|(_, seq)| *seq)
                })
                .expect("real load sequence");
            let ArtifactSource::Value(model) = self.provider.beat_grid(index) else {
                panic!("fixture grid must be materialized")
            };
            let target = deck.id();
            self.host
                .send(LinkedHostCommand::Grid {
                    deck: target,
                    answer: GridAnswer {
                        item,
                        load,
                        model: Ok((*model).clone()),
                    },
                })
                .await
                .unwrap_or_else(|error| panic!("{}: deliver grid {index}: {error}", case.id));
            hang_reset!();
        }
    }

    pub(crate) async fn physical_failures(
        &mut self,
        case: SyncCase,
        deck: usize,
        pcm: &[f32],
        wrong_source: &kithara_integration_tests::pcm_oracle::Audio,
        jump: Option<(SessionFrame, SessionFrame)>,
    ) -> Vec<String> {
        self.physical_reference(case, deck, pcm, wrong_source, jump)
            .await
            .err()
            .into_iter()
            .collect()
    }

    pub(crate) async fn physical_reference(
        &mut self,
        case: SyncCase,
        deck: usize,
        pcm: &[f32],
        wrong_source: &kithara_integration_tests::pcm_oracle::Audio,
        jump: Option<(SessionFrame, SessionFrame)>,
    ) -> Result<Vec<f32>, String> {
        let selected = &self.decks[deck];
        let item = selected
            .current()
            .expect("selected physical capture item")
            .id;
        if selected
            .track_source(item)
            .as_ref()
            .and_then(TrackSource::uri)
            != Some(self.source_uris[deck].as_str())
        {
            return Err(format!(
                "deck {deck}: selected source differs from the requested provider URI"
            ));
        }
        let ArtifactSource::Value(grid) = self.provider.beat_grid(deck) else {
            panic!("physical capture has a declared source grid");
        };
        let marks: Vec<_> = self
            .capture_marks
            .iter()
            .filter_map(|(frame, owner, slot)| (*owner == item).then_some((*frame, *slot)))
            .collect();
        let decoded = self.decoded[deck].lock().view().clone();
        #[cfg(not(target_os = "android"))]
        let frames = pcm.len() / usize::from(CHANNELS);
        #[cfg(not(target_os = "android"))]
        let ledger_points = marks.len();
        let pcm = pcm.to_vec();
        let wrong_source = kithara_integration_tests::pcm_oracle::Audio {
            samples: wrong_source.samples.clone(),
            channels: wrong_source.channels,
            sample_rate: wrong_source.sample_rate,
        };
        let consumed = super::consumed::Capture::for_track(u64::from(item), case.sample_rate);
        let reference = kithara::platform::tokio::task::spawn_blocking(move || {
            decoded.check_physical(&pcm, &marks, &consumed?, &grid, &wrong_source, jump)
        })
        .await
        .expect("physical PCM reference analysis must join");
        #[cfg(not(target_os = "android"))]
        self.master_tap.evidence(
            "physical_capture",
            serde_json::json!({
                "boundary": "postdecoder/SRC PCM through actual Warp SourceSpan to Master",
                "source": self.source_uris[deck], "item": u64::from(item),
                "mix_gain": super::control::CONTROL_GAIN,
                "frames": frames, "ledger_points": ledger_points,
                "failures": reference.as_ref().err().into_iter().collect::<Vec<_>>(),
            }),
        );
        reference
    }

    pub(crate) async fn command_boundary_failures(
        &self,
        issued: SessionFrame,
        expected: SessionFrame,
        first_speed: usize,
        jump: Option<SessionFrame>,
    ) -> Vec<String> {
        use kithara::play::Settled;
        use kithara_command::When;

        let host_grid = self.host.session_grid().await;
        let source_rate = self.host.spec().await.sample_rate;
        let ArtifactSource::Value(model) = self.provider.beat_grid(0) else {
            panic!("command has a declared source grid");
        };
        let deck = &self.decks[0];
        let mut failures = Vec::new();
        deck.observe(|observation| {
            let speeds = &observation.speeds[first_speed..];
            let [speed] = speeds else {
                failures.push(format!(
                    "one command produced {} lane speed operations",
                    speeds.len()
                ));
                return;
            };
            if speed.at != When::At(expected) {
                failures.push(format!(
                    "lane speed admitted at {:?}, expected {expected:?}",
                    speed.at
                ));
            }
            if speed.landing.map(|(_, frame)| frame) != jump {
                failures.push(format!(
                    "lane jump landing differs: {:?}, expected {jump:?}",
                    speed.landing
                ));
            }
            let applied = observation.settled.iter().any(|(item, receipt)| {
                *item == speed.item
                    && matches!(receipt, Settled::Applied { seq, at }
                    if *seq == speed.seq && *at == expected)
            });
            if !applied {
                failures.push(format!(
                    "lane command {:?} has no exact Applied at {expected:?}",
                    speed.seq
                ));
            }
            let first = self.capture_marks.first().and_then(|(frame, item, slot)| {
                (*frame == issued && *item == speed.item).then_some(*slot)
            });
            if first != speed.consumed {
                failures
                    .push("command capture lost its actual pre-command consumed slot".to_owned());
            }
            let mut selected_jump = None;
            if let Some(landing) = jump {
                let anchor = (|| {
                    let before =
                        first.ok_or_else(|| "jump has no actual consumed source".to_owned())?;
                    let mark = before
                        .mark
                        .ok_or_else(|| "jump has no source mark".to_owned())?;
                    let media_origin = before
                        .source
                        .and_then(|info| info.media_origin)
                        .ok_or_else(|| "jump has no immutable media origin".to_owned())?;
                    let elapsed = Duration::from_secs_f64(
                        (i64::from(landing) - i64::from(issued))
                            .to_f64()
                            .ok_or_else(|| "jump duration is not representable".to_owned())?
                            / f64::from(source_rate.get()),
                    );
                    let projected = mark
                        .position
                        .checked_sub(media_origin)
                        .and_then(|position| position.checked_add(elapsed))
                        .ok_or_else(|| "jump projection overflows the source axis".to_owned())?;
                    let anchor = super::binding::select_nearest(
                        &model,
                        source_rate,
                        &host_grid,
                        projected,
                        landing,
                        4,
                    )?;
                    let (actual, at) = speed
                        .landing
                        .ok_or_else(|| "SYNC command has no actual jump target".to_owned())?;
                    let mut errors =
                        anchor.command_failures(&model, source_rate, &host_grid, actual, at)?;
                    let consumed = observation
                        .consumed
                        .iter()
                        .find_map(|(item, slot)| (*item == speed.item).then_some(*slot))
                        .ok_or_else(|| "jump has no final consumed PCM".to_owned())?;
                    errors.extend(anchor.consumption_failures(
                        &model,
                        source_rate,
                        &host_grid,
                        consumed,
                        mark.lane.segment,
                    )?);
                    Ok::<_, String>((errors, (anchor.media, landing)))
                })();
                match anchor {
                    Ok((errors, target)) => {
                        failures.extend(errors);
                        selected_jump = Some(target);
                    }
                    Err(error) => failures.push(error),
                }
            }
            self.check_command_law(speed, issued, expected, selected_jump, &mut failures);
            self.check_command_consumption(speed, first.as_ref(), issued, expected, &mut failures);
        });
        failures
    }

    fn check_command_law(
        &self,
        speed: &LinkSpeed,
        issued: SessionFrame,
        expected: SessionFrame,
        selected_jump: Option<(Duration, SessionFrame)>,
        failures: &mut Vec<String>,
    ) {
        let marks: Vec<_> = self
            .capture_marks
            .iter()
            .filter_map(|(frame, item, slot)| (*item == speed.item).then_some((*frame, *slot)))
            .collect();
        let mut recording = self.decoded[0].lock();
        let decoded = recording.view();
        match super::binding::commanded_span_failures(
            speed,
            issued,
            expected,
            &marks,
            selected_jump,
            decoded,
        ) {
            Ok(errors) if errors.is_empty() => {
                let late = super::binding::commanded_span_failures(
                    speed,
                    issued,
                    expected + kithara::signal::FrameCount::new(1),
                    &marks,
                    selected_jump.map(|(media, landing)| {
                        (media, landing + kithara::signal::FrameCount::new(1))
                    }),
                    decoded,
                );
                if !matches!(late, Ok(ref errors) if !errors.is_empty()) {
                    failures.push(format!(
                        "command source-law late-frame negative did not reject the PCM: {late:?}"
                    ));
                }
                let shifted = marks
                    .iter()
                    .map(|&(frame, mut slot)| {
                        if let Some(mut info) = slot.source
                            && info.render_revision == speed.seq.get()
                        {
                            info.source_span = Some(
                                info.source_span
                                    .and_then(|span| {
                                        span.scaled_from(
                                            (0, std::num::NonZeroU128::MIN),
                                            (1, std::num::NonZeroU128::MIN),
                                            (1, std::num::NonZeroU128::MIN),
                                        )
                                    })
                                    .ok_or_else(|| {
                                        "command source-law negative has no shifted span".to_owned()
                                    })?,
                            );
                            slot.source = Some(info);
                        }
                        Ok((frame, slot))
                    })
                    .collect::<Result<Vec<_>, String>>()
                    .and_then(|shifted| {
                        super::binding::commanded_span_failures(
                            speed,
                            issued,
                            expected,
                            &shifted,
                            selected_jump,
                            decoded,
                        )
                    });
                drop(recording);
                if !matches!(shifted, Ok(ref errors) if !errors.is_empty()) {
                    failures.push(format!(
                        "command source-law offset negative did not reject the PCM: {shifted:?}"
                    ));
                }
            }
            Ok(errors) => failures.extend(errors),
            Err(error) => failures.push(error),
        }
    }

    fn check_command_consumption(
        &self,
        speed: &LinkSpeed,
        first: Option<&SlotSnapshot>,
        issued: SessionFrame,
        expected: SessionFrame,
        failures: &mut Vec<String>,
    ) {
        let Some(info) = first.and_then(|slot| slot.source) else {
            failures.push("command capture lost its pre-command source identity".to_owned());
            return;
        };
        let Some((end, _, _)) = self.capture_marks.last() else {
            failures.push("command capture lost its final consumed frame".to_owned());
            return;
        };
        let result =
            super::consumed::Capture::for_track(u64::from(speed.item), info.spec.sample_rate.get())
                .and_then(|capture| {
                    capture.command_boundary(
                        i64::from(issued)..i64::from(*end),
                        info.render_revision,
                        speed.seq.get(),
                        i64::from(expected),
                    )
                });
        if let Err(error) = result {
            failures.push(error);
        }
    }

    pub(crate) async fn grid_binding_failures(&mut self, case: SyncCase) -> Vec<String> {
        use kithara::play::TrackStatus as OwnerTrackStatus;
        let host_grid = self.host.session_grid().await;
        let mut failures = Vec::new();
        let mut evidence = Vec::new();
        for (index, deck) in self.decks.iter().enumerate() {
            let item = deck.current().expect("selected binding track").id;
            let (snapshot, slot) = deck
                .read_snapshot()
                .await
                .into_iter()
                .find(|(snapshot, _)| snapshot.track.item == item)
                .expect("real Player snapshot and consumed slot from one DeckPass");
            assert!(matches!(
                snapshot.track.status,
                OwnerTrackStatus::Playing { .. }
            ));
            assert!(
                matches!(
                    snapshot.sync,
                    kithara::link::SyncStatus::On | kithara::link::SyncStatus::Correcting { .. }
                ),
                "a playing synchronized track must expose its steady binding or finite correction"
            );
            let evaluation_rate = deck
                .observe(|observation| observation.consumed_rate)
                .expect("real DeckPass grid evaluation axis");
            assert_eq!(
                snapshot.track.mark, slot.mark,
                "the Player snapshot reads that consumed PCM mark"
            );
            assert_eq!(
                evaluation_rate.get(),
                case.sample_rate,
                "the consumed DeckPass declares the grid evaluation axis"
            );
            let ArtifactSource::Value(model) = self.provider.beat_grid(index) else {
                panic!("materialized supplied source grid");
            };
            let (capture_frame, initial) = self
                .capture_marks
                .iter()
                .find_map(|(frame, owner, slot)| (*owner == item).then_some((*frame, *slot)))
                .expect("unchanged capture's actual initial consumed mark");
            let Some(capture_start) = self.capture_marks.iter().find_map(|(_, owner, slot)| {
                (*owner == item && slot.state == kithara_render::bridge::SlotState::Playing)
                    .then_some(*slot)
            }) else {
                failures.push(format!(
                    "deck {index}: original capture has no actual consumed Playing slot"
                ));
                continue;
            };
            if initial.state != kithara_render::bridge::SlotState::Playing {
                let first = capture_start
                    .mark
                    .expect("actual Playing capture has a consumed mark");
                let started = deck.observe(|observation| {
                    observation.starts.iter().any(|(owner, seq)| {
                        *owner == item
                            && observation.placement_receipts.iter().any(
                                |(owner, origin, receipt)| {
                                    *owner == item
                                        && *origin == LinkReceiptOrigin::Deck
                                        && matches!(receipt, Settled::Applied { seq: answered, at }
                            if answered == seq && *at >= capture_frame && *at <= first.session)
                                },
                            )
                    })
                });
                if !started
                    || initial
                        .source
                        .map(|info| (info.segment, info.source_revision, info.media_origin))
                        != capture_start
                            .source
                            .map(|info| (info.segment, info.source_revision, info.media_origin))
                    || initial
                        .mark
                        .is_some_and(|mark| mark.lane.segment != first.lane.segment)
                {
                    failures.push(format!("deck {index}: initially stopped capture lacks real owned Start Applied custody or stable source origin/Segment"));
                    continue;
                }
            }
            let law = deck.observe(|observation| -> Result<_, String> {
                let mut history_failures = Vec::new();
                super::binding::published_host_history(
                    observation,
                    &self.host_grids,
                    evaluation_rate,
                    &mut history_failures,
                )?;
                let (speed, mut law_failures, evidence, reference) = super::binding::executed_law(
                    &model,
                    &host_grid,
                    observation,
                    &snapshot,
                    capture_start,
                    slot,
                )?;
                law_failures.extend(history_failures);
                Ok((speed, law_failures, evidence, reference))
            });
            let (actual_speed, reference) = match law {
                Ok((speed, law_failures, law_evidence, reference)) => {
                    failures.extend(
                        law_failures
                            .into_iter()
                            .map(|failure| format!("deck {index}: {failure}")),
                    );
                    evidence.push(law_evidence);
                    (speed, reference)
                }
                Err(error) => {
                    failures.push(format!("deck {index}: {error}"));
                    continue;
                }
            };
            if matches!(snapshot.sync, kithara::link::SyncStatus::Correcting { .. }) {
                continue;
            }
            let binding = super::binding::check(
                &model,
                evaluation_rate,
                &host_grid,
                slot,
                actual_speed.as_(),
                4,
                reference,
            );
            match binding {
                Ok(binding) => {
                    failures.extend(
                        binding
                            .failures()
                            .into_iter()
                            .map(|failure| format!("deck {index}: {failure}")),
                    );
                    evidence.push(binding.evidence());
                }
                Err(error) => failures.push(format!("deck {index}: {error}")),
            }
        }
        #[cfg(not(target_os = "android"))]
        self.master_tap
            .evidence("grid_bindings", serde_json::json!(evidence));
        failures
    }

    pub(crate) async fn grid_anchor_failures(
        &mut self,
        case: SyncCase,
        required_placements: Option<usize>,
    ) -> Vec<String> {
        use kithara::play::Settled;
        use kithara_integration_tests::offline::linked::LinkReceiptOrigin;

        let host_grid = self.host.session_grid().await;
        let Some(selection_grid) = self.operation_grid.as_ref() else {
            return vec!["no published Host grid was captured before the operations".to_owned()];
        };
        let mut failures = Vec::new();
        let mut evidence = Vec::new();
        let stagger_seconds = 3.0 / 8.0 * 60.0 / case.start_bpm();
        for (index, deck) in self.decks.iter().enumerate() {
            let item = deck.current().expect("selected anchor track").id;
            let ArtifactSource::Value(model) = self.provider.beat_grid(index) else {
                panic!("declared anchor source grid");
            };
            let stagger: f64 = index.as_();
            let requested = Duration::from_secs_f64(self.cues[index] + stagger * stagger_seconds);
            let result = deck.observe(|observation| {
                let evaluation_rate = observation.consumed_rate
                    .ok_or_else(|| "no consumed grid evaluation axis".to_owned())?;
                let consumed = observation.consumed.iter()
                    .find_map(|(owner, slot)| (*owner == item).then_some(*slot))
                    .ok_or_else(|| "no actual consumed anchor slot".to_owned())?;
                let placements = observation.placements.iter()
                    .filter(|placement| placement.item == item).collect::<Vec<_>>();
                let required_placements = super::binding::relocation_count(
                    observation, item, &model, case, required_placements,
                )?;
                if required_placements.is_some_and(|required| placements.len() != required) {
                    return Err(format!(
                        "expected {:?} admitted relocations, found {}",
                        required_placements, placements.len()
                    ));
                }
                let mut bindings = Vec::new();
                for (ordinal, placement) in placements.iter().enumerate() {
                    let required = super::binding::relocation_request(
                        case.order, ordinal + 1 == placements.len(), requested,
                        placement.planned.as_ref()
                            .map(|projected| projected.0)
                            .map_err(|error| format!("pre-emission source projection failed: {error}")),
                    )?;
                    let lead = placement.delivery.get()
                        .checked_add(placement.before.ring_depth.get())
                        .and_then(|lead| lead.checked_add(placement.before.engine_latency.get()))
                        .and_then(|lead| lead.checked_add(placement.before.declick.get()))
                        .ok_or_else(|| "placement lead does not fit the Host frame axis".to_owned())?;
                    let expected_at = placement.now + kithara::signal::FrameCount::new(lead);
                    let anchor = super::binding::select_nearest(
                        &model, evaluation_rate, selection_grid, required, expected_at, 4,
                    )?;
                    let errors = anchor.command_failures(
                        &model, evaluation_rate, selection_grid, placement.to, placement.at,
                    )?;
                    if !errors.is_empty() {
                        return Err(format!("placement {:?}: {}", placement.seq, errors.join("; ")));
                    }
                    let admitted_at = anchor.frame - placement.before.declick;
                    let applied = observation.placement_receipts.iter()
                        .filter(|(owner, origin, settled)| {
                            *owner == item && *origin == LinkReceiptOrigin::Lane
                                && matches!(settled, Settled::Applied { seq, at }
                                    if *seq == placement.seq && *at == admitted_at)
                        }).count();
                    if placement.aligned && applied != 1 {
                        return Err(format!("relocation {:?} has {applied} owned Lane Applied receipts at {:?}", placement.seq, admitted_at));
                    }
                    if !placement.aligned {
                        let applied = observation.placement_applied.iter()
                            .find_map(|(owner, seq, applied)| (*owner == item && *seq == placement.seq).then_some(*applied));
                        if applied != Some(true) {
                            return Err(format!("Jump {:?} has no owned Applied outcome: {:?}", placement.seq, applied));
                        }
                    }
                    let segment = placement.consumed.mark
                        .ok_or_else(|| "admitted relocation has no prior consumed segment".to_owned())?.lane.segment;
                    if ordinal + 1 == placements.len() {
                        let status = observation.tracks.iter().find(|track| track.track.item == item)
                            .ok_or_else(|| "selected relocation has no published Linked snapshot".to_owned())?.sync;
                        let errors = if matches!(status, kithara::link::SyncStatus::Correcting { .. }) {
                            anchor.corrected_consumption_failures(&model, &host_grid, consumed, segment, observation, item)?
                        } else {
                            anchor.consumption_failures(&model, evaluation_rate, &host_grid, consumed, segment)?
                        };
                        if !errors.is_empty() {
                            return Err(errors.join("; "));
                        }
                    }
                    bindings.push(serde_json::json!({
                        "item": u64::from(item), "seq": placement.seq.get(),
                        "requested_media_ns": required.as_nanos().to_string(),
                        "selected_media_ns": anchor.media.as_nanos().to_string(),
                        "source_beat": anchor.source_beat, "host_beat": anchor.host_beat,
                        "target_frame": i64::from(anchor.frame),
                        "scheduled_lane_frame": i64::from(admitted_at), "segment": segment.get(),
                        "lane_applied_frame": placement.aligned.then_some(i64::from(admitted_at)),
                        "aligned": placement.aligned,
                        "owned_applied": observation.placement_applied.iter().find_map(|(owner, seq, applied)|
                            (*owner == item && *seq == placement.seq).then_some(*applied)),
                        "evaluation_rate": evaluation_rate.get(),
                        "selection_grid": format!("{:?}", selection_grid.stamp()),
                        "consumption_grid": format!("{:?}", host_grid.stamp()),
                        "decoder_rates": self.decoder_rates[index],
                        "consumed_mark": format!("{:?}", consumed.mark),
                        "media_origin_ns": consumed.source.and_then(|info| info.media_origin).map(|origin| origin.as_nanos().to_string()),
                    }));
                }
                Ok(bindings)
            });
            match result {
                Ok(binding) => evidence.push(binding),
                Err(error) => failures.push(format!("deck {index}: {error}")),
            }
        }
        #[cfg(not(target_os = "android"))]
        self.master_tap.evidence(
            if required_placements.is_some() {
                "grid_anchors_required_placements"
            } else {
                "grid_anchors"
            },
            serde_json::json!(evidence),
        );
        failures
    }

    pub(crate) async fn request_sync(&mut self, case: SyncCase) {
        self.request_sync_intent(case, SyncRequest::On).await;
    }

    #[kithara_test_utils::kithara::hang_watchdog(timeout = Duration::from_secs(5), ctx = String)]
    pub(crate) async fn request_sync_intent(&mut self, case: SyncCase, intent: SyncRequest) {
        // Ruling: SyncIntent::Enable → LinkedHostCommand::Sync{on:true} — spec §4.5–§4.6.
        // Ruling: SyncIntent::Disable → LinkedHostCommand::Sync{on:false} — spec §4.5–§4.6.
        // Ruling: SyncIntent::AlignNow → explicit Sync{on:true}, including a repeated enable — spec §4.5.
        // Ruling: SyncOperation::Sync{source,activation,transport,load} → DeckId-routed Sync; Linked chooses F from its observed mark and lead — spec §3.4, §4.5, §11.2.
        #[cfg(not(target_os = "android"))]
        self.mark(&format!("sync {intent:?}"));
        for index in 0..self.decks.len() {
            hang_tick!(format!("request_sync_intent: deck {index}, {intent:?}"));
            let target = self.decks[index].id();
            let on = !matches!(intent, SyncRequest::Off);
            self.host
                .send(LinkedHostCommand::Sync { deck: target, on })
                .await
                .unwrap_or_else(|error| panic!("{}: sync deck {index}: {error}", case.id));
            if matches!(case.order, OperationOrder::SequentialSync) {
                let _ = self.render(case, self.block_frames).await;
            }
            hang_reset!();
        }
    }

    #[kithara_test_utils::kithara::hang_watchdog(timeout = Duration::from_secs(5), ctx = String)]
    pub(crate) async fn run_operations(&mut self, case: SyncCase) {
        self.operation_grid = Some(self.host.session_grid().await);
        for operation in case.order.operations() {
            hang_tick!(format!("run_operations: {operation:?}"));
            match operation {
                Operation::Play => {
                    self.play_all().await;
                    self.settle(case, 2).await;
                }
                Operation::Seek => self.seek_staggered(case).await,
                Operation::Sync => self.request_sync(case).await,
            }
            hang_reset!();
        }
    }

    #[kithara_test_utils::kithara::hang_watchdog(timeout = Duration::from_secs(5), ctx = String)]
    pub(crate) async fn ride_tempo(&mut self, case: SyncCase) {
        self.wait_playing(case, "tempo_ride_ready").await;
        let steps_per_leg = (case.updates_hz / 2).max(1);
        let mut start = case.start_bpm();
        let mut rendered = 0_u64;
        let mut update = 0_u64;
        for &target in case.ride.points() {
            for step in 1..=steps_per_leg {
                hang_tick!(format!(
                    "{}: tempo step {step}/{steps_per_leg} toward {target}",
                    case.id
                ));
                let fraction = f64::from(step) / f64::from(steps_per_leg);
                let bpm = start + (target - start) * fraction;
                self.set_tempo(case, bpm, false).await;
                update += 1;
                let deadline = update * u64::from(case.sample_rate) / u64::from(case.updates_hz);
                let frames = deadline.saturating_sub(rendered);
                rendered = deadline;
                if frames > 0 {
                    let frames = usize::try_from(frames).expect("tempo interval fits usize");
                    let _ = self.render(case, frames).await;
                }
                hang_reset!();
            }
            start = target;
        }
        self.settle(case, 4).await;
    }

    #[kithara_test_utils::kithara::hang_watchdog(timeout = Duration::from_secs(5), ctx = String)]
    async fn wait_playing(&mut self, case: SyncCase, event: &str) {
        let deadline = Instant::now() + LOAD_TIMEOUT;
        let sample_rate = NonZeroU32::new(case.sample_rate).expect("capture sample rate");
        let declick = i64::try_from(linked_mixer().declick_frames(sample_rate).get())
            .expect("configured declick fits the session axis");
        #[cfg(not(target_os = "android"))]
        let mut first_live_at = None;
        #[cfg(not(target_os = "android"))]
        let mut sampled_live_after_second = false;
        loop {
            hang_tick!(format!(
                "{}: {event} waiting for actual Start and completed declick at {}",
                case.id, self.output_frames
            ));
            #[cfg(not(target_os = "android"))]
            let mut rejected_mark_pair = None;
            let ready: Option<Vec<_>> = self.decks.iter().map(|deck| {
                let item = deck.current()?.id;
                let snapshot = deck.track_snapshot(item)?;
                if snapshot.track.pending_lane
                    || !matches!(snapshot.track.status, kithara::play::TrackStatus::Playing { .. })
                {
                    return None;
                }
                deck.observe(|observation| {
                    let (_, slot) = observation.consumed.iter().find(|(owner, _)| *owner == item)?;
                    let mark = slot.mark?;
                    let info = slot.source?;
                    if slot.state != kithara_render::bridge::SlotState::Playing
                        || slot.gain != 1.0
                        || snapshot.track.mark != Some(mark)
                        || mark.lane.segment != info.segment
                    {
                        #[cfg(not(target_os = "android"))]
                        if snapshot.track.mark != Some(mark) {
                            rejected_mark_pair = Some((item, snapshot.track.mark, mark));
                        }
                        return None;
                    }
                    let &(_, seq) = observation.starts.iter().rev().find(|(owner, _)| *owner == item)?;
                    let slot_id = snapshot.track.slot?;
                    let at = native_start_at(observation, item, seq, slot_id)?;
                    let stable_at = stable_capture_at(observation, item, at, declick)?;
                    if i64::from(mark.session) < stable_at {
                        return None;
                    }
                    let (custody_at, _) = segment_custody(observation, item, slot_id, info.segment)?;
                    let owns_segment = custody_at <= i64::from(mark.session);
                    owns_segment.then(|| serde_json::json!({
                        "item": u64::from(item), "start_seq": seq.get(), "start_applied": at,
                        "declick_frames": declick, "stable_at": stable_at,
                        "consumed_at": i64::from(mark.session), "segment": info.segment.get(), "slot_gain": slot.gain,
                    }))
                })
            }).collect();
            if let Some(_ready) = ready {
                #[cfg(not(target_os = "android"))]
                self.master_tap.evidence(event, serde_json::json!(_ready));
                hang_reset!();
                return;
            }
            #[cfg(not(target_os = "android"))]
            if !sampled_live_after_second {
                let live = self.decks.iter().all(|deck| {
                    let Some(item) = deck.current().map(|entry| entry.id) else {
                        return false;
                    };
                    deck.observe(|observation| {
                        observation.consumed.iter().any(|(owner, slot)| {
                            *owner == item
                                && slot.state == kithara_render::bridge::SlotState::Playing
                                && slot.gain == 1.0
                                && slot.mark.is_some()
                                && slot.source.is_some()
                        })
                    })
                });
                if live {
                    let event = match first_live_at {
                        None => {
                            first_live_at = Some(self.output_frames);
                            Some("capture_readiness_first_live")
                        }
                        Some(first)
                            if self.output_frames.saturating_sub(first)
                                >= u64::from(case.sample_rate) =>
                        {
                            sampled_live_after_second = true;
                            Some("capture_readiness_live_after_one_second")
                        }
                        Some(_) => None,
                    };
                    if let Some(event) = event {
                        self.record_capture_readiness_failure(event, declick, rejected_mark_pair);
                    }
                }
            }
            let within_deadline = Instant::now() < deadline;
            #[cfg(not(target_os = "android"))]
            if !within_deadline {
                self.record_capture_readiness_failure(
                    "capture_readiness_failure",
                    declick,
                    rejected_mark_pair,
                );
            }
            assert!(
                within_deadline,
                "{}: {event} has no real Applied Start/current Segment with a completed configured declick",
                case.id
            );
            let _ = self.render(case, self.block_frames.min(128)).await;
            hang_reset!();
        }
    }

    #[cfg(not(target_os = "android"))]
    fn record_capture_readiness_failure(
        &mut self,
        event: &str,
        declick: i64,
        rejected_mark_pair: Option<(
            TrackId,
            Option<kithara_render::bridge::SlotMark>,
            kithara_render::bridge::SlotMark,
        )>,
    ) {
        let decks: Vec<_> = self.decks.iter().map(|deck| deck.observe(|observation| {
            let item = deck.current().map(|entry| entry.id);
            let track = observation.tracks.iter()
                .find(|track| Some(track.track.item) == item).map(|track| &track.track);
            let slot = observation.consumed.iter()
                .find(|(owner, _)| Some(*owner) == item).map(|(_, slot)| *slot);
            let mark = slot.and_then(|slot| slot.mark);
            let info = slot.and_then(|slot| slot.source);
            let seq = observation.starts.iter().rev()
                .find(|(owner, _)| Some(*owner) == item).map(|(_, seq)| *seq);
            let slot_id = track.and_then(|track| track.slot);
            let at = item.zip(seq).zip(slot_id)
                .and_then(|((item, seq), slot)| native_start_at(observation, item, seq, slot));
            let stable_at = item.zip(at)
                .and_then(|(item, at)| stable_capture_at(observation, item, at, declick));
            let custody = item.zip(info).zip(slot_id)
                .and_then(|((item, info), slot)| segment_custody(observation, item, slot, info.segment));
            let owns_segment = mark.zip(custody.as_ref()).is_some_and(|(mark, (at, _))| {
                *at <= i64::from(mark.session)
            });
            let predicates = [
                ("current_item", item.is_some()),
                ("track_snapshot", track.is_some()),
                ("pending_lane_clear", track.is_some_and(|track| !track.pending_lane)),
                ("owner_playing", track.is_some_and(|track| matches!(track.status, kithara::play::TrackStatus::Playing { .. }))),
                ("consumed_slot", slot.is_some()),
                ("consumed_mark", mark.is_some()),
                ("source_payload", info.is_some()),
                ("consumed_playing", slot.is_some_and(|slot| slot.state == kithara_render::bridge::SlotState::Playing)),
                ("unity_gain", slot.is_some_and(|slot| slot.gain == 1.0)),
                ("matching_marks", track.zip(mark).is_some_and(|(track, mark)| track.mark == Some(mark))),
                ("matching_segment", mark.zip(info).is_some_and(|(mark, info)| mark.lane.segment == info.segment)),
                ("actual_start", seq.is_some()),
                ("applied_start", at.is_some()),
                ("declick_complete", mark.zip(stable_at).is_some_and(|(mark, stable)| i64::from(mark.session) >= stable)),
                ("current_segment_custody", owns_segment),
            ];
            let failed: Vec<_> = predicates.iter().filter_map(|(name, passed)| (!*passed).then_some(*name)).collect();
            let receipts: Vec<_> = observation.placement_receipts.iter().rev()
                .filter(|(owner, origin, _)| Some(*owner) == item && *origin == LinkReceiptOrigin::Deck)
                .take(6).map(|(_, _, receipt)| format!("{receipt:?}")).collect();
            let start_outcomes: Vec<_> = observation.timeline.iter()
                .filter(|entry| entry["event"].as_str() == Some("deck receipt")
                    && entry["item"].as_u64() == item.map(u64::from)
                    && entry["details"]["seq"].as_u64() == seq.map(Seq::get))
                .map(|entry| &entry["details"]).collect();
            let latest_start = seq.and_then(|seq| observation.timeline.iter().rev().find(|entry| {
                entry.get("event").and_then(serde_json::Value::as_str) == Some("deck receipt")
                    && entry.get("item").and_then(serde_json::Value::as_u64) == item.map(u64::from)
                    && entry.get("details").and_then(|details| details.get("seq")).and_then(serde_json::Value::as_u64) == Some(seq.get())
            })).and_then(|entry| entry.get("details"));
            serde_json::json!({
                "deck": u64::from(deck.id()), "item": item.map(u64::from),
                "failed_predicates": failed, "predicates": predicates,
                "track": format!("{track:?}"), "slot": format!("{slot:?}"),
                "latest_start_seq": seq.map(Seq::get), "latest_start_receipt": latest_start,
                "start_applied": at, "start_outcomes": start_outcomes,
                "stable_at": stable_at, "declick_frames": declick,
                "current_segment_custody": custody.iter().map(|(_, proof)| proof).collect::<Vec<_>>(), "latest_deck_receipts": receipts,
            })
        })).collect();
        self.master_tap.evidence(event, serde_json::json!({
            "output_frame": self.output_frames,
            "rejected_mark_pair": rejected_mark_pair.map(|(item, track, consumed)| serde_json::json!({
                "item": u64::from(item), "track": format!("{track:?}"), "consumed": format!("{consumed:?}"),
            })),
            "decks": decks,
        }));
    }

    pub(super) async fn capture(&mut self, case: SyncCase) -> Vec<f32> {
        self.wait_playing(case, "capture_ready").await;
        let capture_frames: usize = (f64::from(case.sample_rate) * 60.0 / case.ride.final_bpm()
            * 6.0)
            .round()
            .as_();
        self.capture_blocks(
            case,
            capture_frames,
            self.block_frames.min(128),
            CaptureMode::SourceBoundaries,
        )
        .await
    }

    pub(crate) async fn capture_source_frames(
        &mut self,
        case: SyncCase,
        capture_frames: usize,
        block_frames: usize,
    ) -> Vec<f32> {
        self.capture_blocks(
            case,
            capture_frames,
            block_frames,
            CaptureMode::SourceBoundaries,
        )
        .await
    }

    pub(crate) async fn capture_frames(
        &mut self,
        case: SyncCase,
        capture_frames: usize,
        block_frames: usize,
    ) -> Vec<f32> {
        self.capture_blocks(case, capture_frames, block_frames, CaptureMode::Fixed)
            .await
    }

    pub(crate) async fn capture_paced(
        &mut self,
        case: SyncCase,
        capture_frames: usize,
    ) -> Vec<f32> {
        self.capture_blocks(case, capture_frames, self.block_frames, CaptureMode::Paced)
            .await
    }

    #[kithara_test_utils::kithara::hang_watchdog(timeout = Duration::from_secs(5), ctx = String)]
    async fn capture_blocks(
        &mut self,
        case: SyncCase,
        capture_frames: usize,
        block_frames: usize,
        mode: CaptureMode,
    ) -> Vec<f32> {
        assert!(block_frames > 0, "capture block contains frames");
        let paced = matches!(mode, CaptureMode::Paced);
        let capture_kind = if paced { "paced" } else { "product" };
        let observed_frame = SessionFrame::new(
            i64::try_from(self.output_frames).expect("actual capture frame fits session axis"),
        );
        self.capture_marks = self
            .decks
            .iter()
            .flat_map(|deck| deck.observe(|observation| observation.consumed.clone()))
            .map(|(item, slot)| (observed_frame, item, slot))
            .collect();
        #[cfg(not(target_os = "android"))]
        self.mark("capture start");
        let mut pcm = Vec::with_capacity(capture_frames * usize::from(CHANNELS));
        while pcm.len() < capture_frames * usize::from(CHANNELS) {
            let remaining_frames =
                (capture_frames * usize::from(CHANNELS) - pcm.len()) / usize::from(CHANNELS);
            hang_tick!(format!(
                "{}: capturing at frame {} with {remaining_frames} frames remaining",
                case.id, self.output_frames
            ));
            let mut frames = remaining_frames.min(block_frames);
            if matches!(mode, CaptureMode::SourceBoundaries) {
                let packet_remaining = self
                    .decks
                    .iter()
                    .map(|deck| {
                        let item = deck
                            .current()
                            .expect("capture has a selected Playing item")
                            .id;
                        deck.observe(|observation| {
                            let (_, slot) = observation
                                .consumed
                                .iter()
                                .find(|(owner, _)| *owner == item)
                                .expect("capture has the selected consumed source");
                            assert_eq!(slot.state, kithara_render::bridge::SlotState::Playing);
                            let mark = slot.mark.expect("capture has a consumed lane mark");
                            let info = slot.source.expect("capture has consumed source metadata");
                            assert_eq!(mark.lane.segment, info.segment);
                            let span = info.source_span.expect("capture has an exact source span");
                            info.lane_frame
                                .checked_add(span.output_frames())
                                .and_then(|end| end.checked_sub(mark.lane.frame))
                                .expect("capture mark lies within its consumed source span")
                                .max(1)
                        })
                    })
                    .min()
                    .expect("product capture has at least one deck");
                frames = frames.min(
                    usize::try_from(packet_remaining).expect("capture packet length fits usize"),
                );
            }
            let started = paced.then(Instant::now);
            let block = self.render(case, frames).await;
            let observed_frame = SessionFrame::new(
                i64::try_from(self.output_frames).expect("actual capture frame fits session axis"),
            );
            self.capture_marks.extend(
                self.decks
                    .iter()
                    .flat_map(|deck| deck.observe(|observation| observation.consumed.clone()))
                    .map(|(item, slot)| (observed_frame, item, slot)),
            );
            assert_eq!(
                block.len(),
                frames * usize::from(CHANNELS),
                "{}: {capture_kind} capture must retain every requested PCM frame",
                case.id,
            );
            pcm.extend(block);
            hang_reset!();
            if let Some(started) = started {
                let frames: f64 = frames.as_();
                let period = Duration::from_secs_f64(frames / f64::from(case.sample_rate));
                time::sleep(period.saturating_sub(started.elapsed())).await;
            }
        }
        #[cfg(not(target_os = "android"))]
        {
            self.mark("capture end");
            let (beats, downbeats) = kithara_integration_tests::cochlea::marked_rhythm_markers(
                &pcm,
                CHANNELS,
                case.sample_rate,
            );
            self.master_tap.evidence(
                "capture_markers",
                serde_json::json!({"beats": beats, "downbeats": downbeats}),
            );
            let mut curves = std::collections::HashMap::new();
            let observations: Vec<_> = self
                .decks
                .iter()
                .map(|deck| {
                    deck.observe(|observation| {
                        verify_speed_references(observation);
                        serde_json::json!({
                            "deck": u64::from(deck.id()),
                            "timeline": observation.timeline,
                            "tracks": format!("{:?}", observation.tracks),
                            "settled": format!("{:?}", observation.settled),
                            "sync_requests": format!("{:?}", observation.sync_requests),
                            "owner_inputs": observation.owner_inputs.iter().enumerate().map(|(index, input)| serde_json::json!({
                                "index": index, "item": input.item.map(u64::from), "at": input.at.map(i64::from),
                                "kind": format!("{:?}", input.kind), "trajectory": input.trajectory.as_ref().map(|host| format!("{host:?}")),
                            })).collect::<Vec<_>>(),
                            "speed_inputs": observation.speeds.iter().map(|speed| serde_json::json!({
                                "item": u64::from(speed.item), "seq": speed.seq.get(), "at": format!("{:?}", speed.at),
                                "curve": curve_evidence(&mut curves, &speed.curve),
                                "input": speed.input, "host_input": speed.host_input, "planned": format!("{:?}", speed.planned),
                                "landing": format!("{:?}", speed.landing), "consumed": format!("{:?}", speed.consumed),
                            })).collect::<Vec<_>>(),
                            "cue_inputs": observation.cue_curves.iter().map(|cue| serde_json::json!({
                                "item": u64::from(cue.item), "seq": cue.seq.get(), "position_ns": cue.position.as_nanos().to_string(),
                                "host_input": cue.host_input, "consumed": format!("{:?}", cue.consumed),
                                "native_origin": cue.native.map(|origin| format!("{origin:?}")), "adopted_segment": cue.segment.map(SegmentId::get),
                            })).collect::<Vec<_>>(),
                            "capture_facts": self.capture_marks.iter().filter(|(_, item, _)| observation.tracks.iter().any(|track| track.track.item == *item))
                                .map(|(frame, item, slot)| serde_json::json!({"frame": i64::from(*frame), "item": u64::from(*item), "slot": format!("{slot:?}")})).collect::<Vec<_>>(),
                        })
                    })
                })
                .collect();
            self.master_tap
                .evidence("observations", serde_json::json!(observations));
        }
        pcm
    }
}
