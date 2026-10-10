use std::num::NonZeroU32;

use kithara::{
    audio::{
        AudioConfig, AudioControl, AudioEvent, AudioRead, AudioReadError, AudioSession,
        DecoderEvent, ReadOutcome,
    },
    events::EventReceiver,
    file::{File, FileConfig},
    hls::{AbrMode, Hls, HlsConfig},
    platform::{
        time::{self, Duration},
        tokio::sync::broadcast::error::TryRecvError,
    },
    play::{PlayWorker, PlaybackResamplerBackend, SeekOutcome, TrackConfig},
    signal::{AudioSpec, SegmentId},
    stream::Stream,
};
use kithara_integration_tests::{
    event::TestEvent,
    memory_asset_store,
    mock::{LaneAudio, load_audio, wait_for_preload},
};
use kithara_test_utils::bufpool::TestPools;

pub(super) enum ReferenceAudio {
    File(LaneAudio<Stream<File<TestPools>>, TestPools>),
    Hls(LaneAudio<Stream<Hls<TestPools>>, TestPools>),
}

impl ReferenceAudio {
    pub(super) fn subscribe(&self) -> EventReceiver<TestEvent> {
        match self {
            Self::File(audio) => audio.event_bus().subscribe(),
            Self::Hls(audio) => audio.event_bus().subscribe(),
        }
    }
    fn spec(&self) -> AudioSpec {
        match self {
            Self::File(audio) => audio.spec(),
            Self::Hls(audio) => audio.spec(),
        }
    }
    fn segment(&self) -> SegmentId {
        match self {
            Self::File(audio) => audio.segment(),
            Self::Hls(audio) => audio.segment(),
        }
    }
    fn committed_segment(&self) -> Option<SegmentId> {
        match self {
            Self::File(audio) => audio.committed_segment(),
            Self::Hls(audio) => audio.committed_segment(),
        }
    }
    fn seek(&mut self, target: Duration) -> Result<SeekOutcome, AudioReadError> {
        match self {
            Self::File(audio) => audio.seek(target),
            Self::Hls(audio) => audio.seek(target),
        }
    }
    async fn preload(&mut self) -> Result<(), AudioReadError> {
        match self {
            Self::File(audio) => {
                wait_for_preload(audio, "file reference").await;
                audio.preload()
            }
            Self::Hls(audio) => {
                wait_for_preload(audio, "HLS reference").await;
                audio.preload()
            }
        }
    }
    fn read_planar<'a>(
        &mut self,
        output: &'a mut [&'a mut [f32]],
    ) -> Result<ReadOutcome, AudioReadError> {
        match self {
            Self::File(audio) => audio.read_planar(output),
            Self::Hls(audio) => audio.read_planar(output),
        }
    }
}

#[kithara_integration_tests::kithara::flash(io)]
pub(super) async fn open_reference(
    worker: &PlayWorker<TestPools>,
    src: &str,
    hls: bool,
    rate: u32,
) -> ReferenceAudio {
    let sample_rate = NonZeroU32::new(rate).expect("reference rate");
    let bus = kithara::events::EventBus::new(16_384);
    if hls {
        let stream = HlsConfig::for_url(url::Url::parse(src).expect("HLS source URL"))
            .store(memory_asset_store())
            .pools(worker.pools().clone())
            .events(bus)
            .initial_abr_mode(AbrMode::manual(0))
            .build();
        let config = TrackConfig::for_audio(
            AudioConfig::<Hls<TestPools>, PlaybackResamplerBackend>::for_stream(stream)
                .host_sample_rate(sample_rate)
                .build(),
        )
        .block_on_underrun(true)
        .build();
        ReferenceAudio::Hls(
            load_audio(worker, config)
                .await
                .expect("HLS reference lane"),
        )
    } else {
        let stream = FileConfig::for_src(kithara::file::FileSrc::Local(src.into()))
            .store(memory_asset_store())
            .pools(worker.pools().clone())
            .events(bus)
            .build();
        let config = TrackConfig::for_audio(
            AudioConfig::<File<TestPools>, PlaybackResamplerBackend>::for_stream(stream)
                .host_sample_rate(sample_rate)
                .hint("mp3".to_owned())
                .build(),
        )
        .block_on_underrun(true)
        .build();
        ReferenceAudio::File(
            load_audio(worker, config)
                .await
                .expect("file reference lane"),
        )
    }
}

use super::{BLOCK_FRAMES, CHANNELS, CapturedAudio, Case, Deck, PRELOAD_TIMEOUT};

pub(super) async fn capture_references(
    case: &Case,
    decks: &mut [Deck],
    capture: &CapturedAudio,
    failures: &mut Vec<String>,
) -> Vec<Vec<f32>> {
    if capture.start_positions_secs.len() != decks.len() {
        failures.push(format!(
            "{}: final capture recorded {} starts for {} decks",
            case.label,
            capture.start_positions_secs.len(),
            decks.len(),
        ));
        return Vec::new();
    }

    let mut references = Vec::with_capacity(decks.len());
    for (deck_index, (deck, start)) in decks
        .iter_mut()
        .zip(capture.start_positions_secs.iter().copied())
        .enumerate()
    {
        let target = Duration::from_secs_f64(start);
        drain_reference_events(&mut deck.reference_events);
        let seek_ready = match deck.reference.seek(target) {
            Ok(SeekOutcome::Landed { .. }) => true,
            Ok(SeekOutcome::PastEof { duration, .. }) => {
                failures.push(format!(
                    "{} deck {deck_index} reference start {start:.9}s is past EOF at {:.9}s",
                    case.label,
                    duration.as_secs_f64(),
                ));
                false
            }
            Err(error) => {
                failures.push(format!(
                    "{} deck {deck_index} reference seek failed: {error}",
                    case.label,
                ));
                false
            }
        };
        if !seek_ready {
            references.push(Vec::new());
            continue;
        }
        let preload = time::timeout(PRELOAD_TIMEOUT, deck.reference.preload()).await;
        match preload {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                failures.push(format!(
                    "{} deck {deck_index} reference preload failed: {error}",
                    case.label,
                ));
                references.push(Vec::new());
                continue;
            }
            Err(_) => {
                failures.push(format!(
                    "{} deck {deck_index} reference preload timed out",
                    case.label,
                ));
                references.push(Vec::new());
                continue;
            }
        }

        match time::timeout(
            PRELOAD_TIMEOUT,
            read_reference_pcm(
                &mut deck.reference,
                &mut deck.reference_events,
                capture.requested_frames,
                case.source_channels,
            ),
        )
        .await
        {
            Ok(Ok(pcm)) => references.push(pcm),
            Ok(Err(error)) => {
                failures.push(format!(
                    "{} deck {deck_index} reference read failed: {error}",
                    case.label,
                ));
                references.push(Vec::new());
            }
            Err(_) => {
                failures.push(format!(
                    "{} deck {deck_index} reference read timed out",
                    case.label,
                ));
                references.push(Vec::new());
            }
        }
    }
    references
}

async fn read_reference_pcm(
    resource: &mut ReferenceAudio,
    events: &mut EventReceiver<TestEvent>,
    requested_frames: usize,
    source_channels: u16,
) -> Result<Vec<f32>, String> {
    // The reference decodes the source's channels and reads them into
    // `CHANNELS` planes, the same shape the engine renders. A source that is
    // not the one the case declares would compare against the wrong content.
    if resource.spec().channels != source_channels {
        return Err(format!(
            "reference has {} channels, expected {source_channels}",
            resource.spec().channels,
        ));
    }
    let mut pcm = Vec::with_capacity(requested_frames * usize::from(CHANNELS));
    let requested_segment = resource.segment();
    let mut left = vec![0.0; BLOCK_FRAMES];
    let mut right = vec![0.0; BLOCK_FRAMES];
    while pcm.len() / usize::from(CHANNELS) < requested_frames {
        let completed = pcm.len() / usize::from(CHANNELS);
        let frames = (requested_frames - completed).min(BLOCK_FRAMES);
        let mut planar = [&mut left[..frames], &mut right[..frames]];
        match resource
            .read_planar(&mut planar)
            .map_err(|error| error.to_string())?
        {
            ReadOutcome::Frames { count, .. } => {
                drain_reference_seek_events(events)?;
                if pcm.is_empty() {
                    validate_reference_seek_barrier(
                        Some(requested_segment),
                        resource.committed_segment(),
                    )?;
                }
                let count = count.get();
                for frame in 0..count {
                    pcm.push(left[frame]);
                    pcm.push(right[frame]);
                }
            }
            ReadOutcome::Pending { .. } => time::sleep(Duration::from_millis(1)).await,
            ReadOutcome::Eof { position } => {
                return Err(format!(
                    "reference reached EOF at {:.9}s after {completed}/{requested_frames} frames",
                    position.as_secs_f64(),
                ));
            }
        }
        drain_reference_seek_events(events)?;
    }
    Ok(pcm)
}

fn validate_reference_seek_barrier(
    requested_segment: Option<SegmentId>,
    completion: Option<SegmentId>,
) -> Result<(), String> {
    let requested_segment =
        requested_segment.ok_or_else(|| "reference seek request missing".to_owned())?;
    let completed_segment = completion.ok_or_else(|| {
        format!("reference seek segment {requested_segment:?} did not complete with its first PCM")
    })?;
    if completed_segment != requested_segment {
        return Err(format!(
            "reference completed seek segment {completed_segment:?}, expected {requested_segment:?}",
        ));
    }
    Ok(())
}

fn drain_reference_events(events: &mut EventReceiver<TestEvent>) {
    while let Ok(_) | Err(TryRecvError::Lagged(_)) = events.try_recv() {}
}

fn drain_reference_seek_events(events: &mut EventReceiver<TestEvent>) -> Result<(), String> {
    loop {
        match events.try_recv() {
            Ok(envelope) => match envelope.event {
                TestEvent::Audio(AudioEvent::SeekRejected { target }) => {
                    return Err(format!(
                        "reference rejected seek to {:.9}s",
                        target.as_secs_f64()
                    ));
                }
                TestEvent::Audio(AudioEvent::TrackFailed { failure, .. }) => {
                    return Err(format!("reference track failed: {failure:?}"));
                }
                TestEvent::Decoder(DecoderEvent::DecodeError { detail, .. }) => {
                    return Err(format!("reference decode failed: {detail}"));
                }
                _ => {}
            },
            Err(TryRecvError::Empty) => return Ok(()),
            Err(TryRecvError::Lagged(count)) => {
                return Err(format!("reference event receiver lost {count} events"));
            }
            Err(TryRecvError::Closed) => return Err("reference event receiver closed".to_owned()),
        }
    }
}
