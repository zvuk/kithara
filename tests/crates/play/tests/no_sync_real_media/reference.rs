use kithara::{
    audio::ReadOutcome,
    events::{AudioEvent, DecoderEvent, Event, EventReceiver, SeekLifecycleStage},
    platform::{
        time::{self, Duration},
        tokio::sync::broadcast::error::{RecvError, TryRecvError},
    },
    play::{Resource, SeekOutcome},
};

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
    resource: &mut Resource,
    events: &mut EventReceiver,
    requested_frames: usize,
) -> Result<Vec<f32>, String> {
    if resource.spec().channels != CHANNELS {
        return Err(format!(
            "reference has {} channels, expected {CHANNELS}",
            resource.spec().channels,
        ));
    }
    let mut pcm = Vec::with_capacity(requested_frames * usize::from(CHANNELS));
    let mut request_epoch = None;
    let mut completion = None;
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
                drain_reference_seek_events(events, &mut request_epoch, &mut completion)?;
                if pcm.is_empty() {
                    wait_for_reference_seek_completion(events, &mut request_epoch, &mut completion)
                        .await?;
                    validate_reference_seek_barrier(request_epoch, completion)?;
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
        drain_reference_seek_events(events, &mut request_epoch, &mut completion)?;
    }
    Ok(pcm)
}

async fn wait_for_reference_seek_completion(
    events: &mut EventReceiver,
    request_epoch: &mut Option<u64>,
    completion: &mut Option<u64>,
) -> Result<(), String> {
    time::timeout(PRELOAD_TIMEOUT, async {
        while completion.is_none() {
            let envelope = events.recv().await.map_err(|error| match error {
                RecvError::Lagged(count) => {
                    format!("reference event receiver lost {count} events")
                }
                RecvError::Closed => "reference event receiver closed".to_owned(),
            })?;
            observe_reference_seek_event(envelope.event, request_epoch, completion)?;
        }
        Ok(())
    })
    .await
    .map_err(|_| "reference seek completion timed out".to_owned())?
}

fn validate_reference_seek_barrier(
    request_epoch: Option<u64>,
    completion: Option<u64>,
) -> Result<(), String> {
    let request_epoch = request_epoch.ok_or_else(|| "reference seek request missing".to_owned())?;
    let complete_epoch = completion.ok_or_else(|| {
        format!("reference seek epoch {request_epoch} did not complete with its first PCM")
    })?;
    if complete_epoch != request_epoch {
        return Err(format!(
            "reference completed seek epoch {complete_epoch}, expected {request_epoch}",
        ));
    }
    Ok(())
}

fn drain_reference_events(events: &mut EventReceiver) {
    loop {
        match events.try_recv() {
            Ok(_) | Err(TryRecvError::Lagged(_)) => {}
            Err(TryRecvError::Empty | TryRecvError::Closed) => break,
        }
    }
}

fn drain_reference_seek_events(
    events: &mut EventReceiver,
    request_epoch: &mut Option<u64>,
    completion: &mut Option<u64>,
) -> Result<(), String> {
    loop {
        match events.try_recv() {
            Ok(envelope) => {
                observe_reference_seek_event(envelope.event, request_epoch, completion)?;
            }
            Err(TryRecvError::Empty) => return Ok(()),
            Err(TryRecvError::Lagged(count)) => {
                return Err(format!("reference event receiver lost {count} events"));
            }
            Err(TryRecvError::Closed) => return Err("reference event receiver closed".to_owned()),
        }
    }
}

fn observe_reference_seek_event(
    event: Event,
    request_epoch: &mut Option<u64>,
    completion: &mut Option<u64>,
) -> Result<(), String> {
    match event {
        Event::Audio(AudioEvent::SeekLifecycle {
            stage: SeekLifecycleStage::SeekRequest,
            seek_epoch,
            ..
        }) => *request_epoch = Some(seek_epoch),
        Event::Audio(AudioEvent::SeekComplete { seek_epoch, .. }) => *completion = Some(seek_epoch),
        Event::Audio(AudioEvent::SeekRejected { epoch, target }) => {
            return Err(format!(
                "reference rejected seek epoch {epoch} to {:.9}s",
                target.as_secs_f64(),
            ));
        }
        Event::Audio(AudioEvent::TrackFailed { failure, .. }) => {
            return Err(format!("reference track failed: {failure:?}"));
        }
        Event::Decoder(DecoderEvent::DecodeError { detail, .. }) => {
            return Err(format!("reference decode failed: {detail}"));
        }
        _ => {}
    }
    Ok(())
}
