#![cfg(not(target_arch = "wasm32"))]

use kithara::{
    beat::{BeatGridModel, BeatGridState},
    events::TrackId,
    link::{GridAnswer, LinkConfig, LinkedHostCommand, SyncStatus},
    platform::time::Duration,
    sync::SpeedCurve,
    warp::{BeatGridQuery, MapPoint, MapPosition},
};
use kithara_command::When;
use kithara_integration_tests::{grid::Start, kithara, offline::linked::LinkReceiptOrigin};
use num_traits::ToPrimitive;

use super::sync_product_matrix::{
    Audible, BLOCK_FRAMES, ONE_DECK, PreparedSources, ProductHarness, SyncCase, synthetic_sources,
};

#[kithara::test(native, tokio, multi_thread, serial, timeout(Duration::from_secs(60)))]
async fn progressive_analysis_preserves_consumed_audio_and_explicit_seek_lands_at_once(
    #[future(awt)] synthetic_sources: PreparedSources,
) {
    let case = ONE_DECK;
    let mut rig = ProductHarness::new(
        case,
        &synthetic_sources,
        Start::Seconds(0.0),
        Audible::Deck(0),
    )
    .await;
    rig.request_sync(case).await;
    rig.settle(case, 32).await;
    let pcm = rig
        .render(
            case,
            usize::try_from(case.sample_rate).expect("sample rate fits usize"),
        )
        .await;
    assert!(pcm.iter().any(|sample| sample.abs() > 0.01));
    let item = rig.decks[0].current().expect("sounding track").id;
    let target = rig.decks[0].id();
    let mut snapshots = rig.decks[0].read_snapshot().await;
    assert_eq!(snapshots.len(), 1);
    let (snapshot, slot) = snapshots.pop().expect("sounding track snapshot");
    assert_eq!(snapshot.track.item, item);
    let mut previous = slot.mark.expect("consumed PCM mark before analysis");
    assert_eq!(snapshot.track.mark, Some(previous));
    let origin = slot
        .source
        .expect("consumed source clock")
        .media_origin
        .expect("known media origin");
    assert_eq!(snapshot.track.position, previous.position - origin);
    let placements = rig.decks[0].observe(|observation| observation.placements.len());
    let kithara::play::ArtifactSource::Value(authored) = rig.provider_grid(0) else {
        panic!("materialized authored grid");
    };
    let tolerance = 2.0 * f64::from(LinkConfig::default().epsilon());
    for (revision, shift, beats) in [(2, 0.125, 12), (3, 0.06, 16), (4, 0.18, 20), (5, 0.0, 24)] {
        let mut raw = authored.as_raw().clone();
        raw.revision = revision;
        if revision < 5 {
            raw.state = BeatGridState::Provisional;
            raw.beats.truncate(beats);
            raw.meter = None;
            raw.downbeats.clear();
            for beat in &mut raw.beats {
                beat.at += shift;
            }
        }
        let model = BeatGridModel::try_from(raw).expect("valid progressive analysis");
        assert!(kithara::sync::covers(
            model.geometry(),
            previous.position - origin
        ));
        rig.host
            .send(LinkedHostCommand::Grid {
                deck: target,
                answer: GridAnswer {
                    item,
                    model: Ok(model),
                },
            })
            .await
            .expect("analysis reaches the sounding deck");
        for _ in 0..96 {
            let _ = rig.render(case, BLOCK_FRAMES).await;
            let mut snapshots = rig.decks[0].read_snapshot().await;
            assert_eq!(snapshots.len(), 1);
            let (snapshot, slot) = snapshots.pop().expect("current consumed snapshot");
            let mark = slot.mark.expect("analysis preserves consumed PCM");
            assert_eq!(snapshot.track.mark, Some(mark));
            assert_eq!(mark.lane.segment, previous.lane.segment);
            let frames = mark
                .session
                .frames_since(previous.session)
                .expect("the output clock advances");
            assert_eq!(
                frames,
                u64::try_from(BLOCK_FRAMES).expect("block count fits u64")
            );
            let advance = mark
                .position
                .checked_sub(previous.position)
                .expect("analysis never repeats earlier media")
                .as_secs_f64()
                * f64::from(case.sample_rate);
            let frames = frames.to_f64().expect("small rendered frame count");
            assert!(
                (advance - frames).abs() <= frames * tolerance + 2.0,
                "revision {revision}: consumed {advance} source frames over {frames} output frames"
            );
            assert!(matches!(
                snapshot.sync,
                SyncStatus::On | SyncStatus::Correcting { .. }
            ));
            previous = mark;
        }
        assert_eq!(
            rig.decks[0].observe(|observation| observation.placements.len()),
            placements,
            "analysis and late meter discovery issue no Jump or Align"
        );
    }

    assert_explicit_seek_lands(&mut rig, case, placements, item).await;
}

async fn assert_explicit_seek_lands(
    rig: &mut ProductHarness,
    case: SyncCase,
    placements: usize,
    item: TrackId,
) {
    let speed_inputs = rig.decks[0].observe(|observation| observation.speeds.len());
    let requested = 8.25;
    let control = rig.decks[0].control().clone();
    rig.host
        .run(move || control.seek(requested))
        .await
        .expect("explicit seek reaches the deck");
    rig.settle(case, 32).await;
    let (landing, at, issued, lead, seq) = rig.decks[0].observe(|observation| {
        let moves = &observation.placements[placements..];
        assert_eq!(moves.len(), 1, "the explicit seek causes one placement");
        let placement = &moves[0];
        let lead = placement.before.ring_depth.get()
            + placement.before.engine_latency.get()
            + placement.before.declick.get()
            + placement.delivery.get();
        (
            placement.to,
            placement.at,
            placement.now,
            lead,
            placement.seq,
        )
    });
    assert!(
        at.frames_since(issued).expect("future seek landing")
            <= u64::try_from(lead).expect("seek lead fits u64")
    );
    let grid = rig.host.session_grid().await;
    let BeatGridQuery::Resolved(beat) =
        grid.beat_at(MapPoint::new(grid.stamp(), MapPosition::Session(at)))
    else {
        panic!("the host resolves the seek landing");
    };
    let host_phase = f64::from(*beat.value().value()).rem_euclid(4.0);
    let phase = host_phase + 1.0;
    let earlier = phase + ((requested * 2.0 - phase) / 4.0).floor() * 4.0;
    let expected = [earlier / 2.0, (earlier + 4.0) / 2.0]
        .into_iter()
        .min_by(|a, b| (a - requested).abs().total_cmp(&(b - requested).abs()))
        .expect("nearest matching phase");
    assert!((landing.as_secs_f64() - expected).abs() * f64::from(case.sample_rate) <= 2.0);
    let mut snapshots = rig.decks[0].read_snapshot().await;
    let (snapshot, slot) = snapshots.pop().expect("consumed seek landing");
    let mark = slot.mark.expect("PCM resumes after the seek");
    assert_eq!(snapshot.track.mark, Some(mark));
    let origin = slot
        .source
        .expect("consumed source clock")
        .media_origin
        .expect("media origin");
    let media = mark.position.checked_sub(origin).expect("media position");
    let elapsed = mark
        .session
        .frames_since(at)
        .expect("seek already took effect")
        .to_f64()
        .expect("small seek settlement span")
        / f64::from(case.sample_rate);
    assert!(
        (media.as_secs_f64() - landing.as_secs_f64() - elapsed).abs() * f64::from(case.sample_rate)
            <= 2.0
    );
    rig.decks[0].observe(|observation| {
        assert!(
            observation
                .placement_applied
                .iter()
                .any(|&(owner, answered, applied)| owner == item && answered == seq && applied)
        );
        assert!(
            observation.speeds[speed_inputs..].iter().all(|speed| {
                speed.landing.is_some()
                    || matches!(
                        (&speed.curve, speed.at),
                        (SpeedCurve::Constant(1.0), When::At(_))
                    )
            }),
            "an explicit seek leaves no phase catch-up behind"
        );
    });
    assert!(rig.failures.is_empty(), "{:?}", rig.failures);
}

#[kithara::test(native, tokio, multi_thread, serial, timeout(Duration::from_secs(60)))]
async fn a_manual_seek_replaces_an_applied_source_programme_with_the_same_tail_speed(
    #[future(awt)] synthetic_sources: PreparedSources,
) {
    let case = ONE_DECK;
    let mut rig = ProductHarness::new(
        case,
        &synthetic_sources,
        Start::Seconds(0.0),
        Audible::Deck(0),
    )
    .await;
    let item = rig.decks[0].current().expect("real loaded track").id;
    let target = rig.decks[0].id();
    let kithara::play::ArtifactSource::Value(authored) = rig.provider_grid(0) else {
        panic!("materialized source grid");
    };
    let mut raw = authored.as_raw().clone();
    raw.revision += 1;
    raw.meter = None;
    raw.downbeats.clear();
    raw.beats.truncate(22);
    for beat in &mut raw.beats {
        let ordinal = beat.ordinal.to_f64().expect("small beat ordinal");
        beat.at = if ordinal <= 6.0 {
            ordinal * 0.5
        } else if ordinal <= 9.0 {
            3.0 + ordinal - 6.0
        } else {
            6.0 + (ordinal - 9.0) * 0.5
        };
    }
    let model = BeatGridModel::try_from(raw).expect("120 to 60 to 120 BPM source geometry");
    rig.host
        .send(LinkedHostCommand::Grid {
            deck: target,
            answer: GridAnswer {
                item,
                model: Ok(model),
            },
        })
        .await
        .expect("source programme reaches its owner");
    let pcm = rig
        .render(
            case,
            usize::try_from(case.sample_rate).expect("sample rate"),
        )
        .await;
    assert!(
        pcm.iter().any(|sample| sample.abs() > 0.01),
        "the source already sounds before SYNC"
    );
    rig.request_sync(case).await;
    rig.settle(case, 32).await;
    let (speeds, placements) = assert_applied_source_programme(&mut rig, case, item).await;
    let requested = 8.0;
    let control = rig.decks[0].control().clone();
    rig.host
        .run(move || control.seek(requested))
        .await
        .expect("explicit seek");
    rig.settle(case, 32).await;
    let (landing, at) = rig.decks[0].observe(|observation| {
        let new_speeds = &observation.speeds[speeds..];
        assert_eq!(
            new_speeds.len(),
            1,
            "a seek replaces the applied future programme even when its tail equals the new speed"
        );
        assert_eq!(new_speeds[0].curve, SpeedCurve::Constant(1.0));
        let moves = &observation.placements[placements..];
        assert_eq!(moves.len(), 1);
        assert_eq!(new_speeds[0].seq, moves[0].seq);
        assert!(moves[0].aligned);
        (moves[0].to, moves[0].at)
    });
    let host = rig.host.session_grid().await;
    let BeatGridQuery::Resolved(beat) =
        host.beat_at(MapPoint::new(host.stamp(), MapPosition::Session(at)))
    else {
        panic!("Host phase at seek");
    };
    let phase = f64::from(*beat.value().value()).rem_euclid(1.0);
    let earlier = (phase + (requested * 2.0 - phase).floor()) * 0.5;
    let expected = [earlier, earlier + 0.5]
        .into_iter()
        .min_by(|a, b| (a - requested).abs().total_cmp(&(b - requested).abs()))
        .expect("closest source phase");
    assert!((landing.as_secs_f64() - expected).abs() * f64::from(case.sample_rate) <= 2.0);
    rig.settle(case, 240).await;
    let (snapshot, slot) = rig.decks[0]
        .read_snapshot()
        .await
        .pop()
        .expect("source continues beyond the old excursion");
    let mark = slot.mark.expect("actual consumed PCM after seek");
    let origin = slot
        .source
        .expect("source clock")
        .media_origin
        .expect("media origin");
    let elapsed = mark
        .session
        .frames_since(at)
        .expect("after seek")
        .to_f64()
        .expect("bounded playout")
        / f64::from(case.sample_rate);
    let media = mark.position.checked_sub(origin).expect("media position");
    assert!(
        elapsed > 2.5,
        "the observation passes the old programme's future middle region"
    );
    assert!(
        (media.as_secs_f64() - landing.as_secs_f64() - elapsed).abs() * f64::from(case.sample_rate)
            <= 2.0,
        "the old faster programme never reappears after the explicit seek"
    );
    assert_eq!(snapshot.track.speed, 1.0);
    assert!(rig.failures.is_empty(), "{:?}", rig.failures);
    drop(rig);
}

async fn assert_applied_source_programme(
    rig: &mut ProductHarness,
    case: SyncCase,
    item: TrackId,
) -> (usize, usize) {
    let (snapshot, slot) = rig.decks[0]
        .read_snapshot()
        .await
        .pop()
        .expect("real consumed source");
    assert_eq!(snapshot.sync, SyncStatus::On);
    assert_eq!(snapshot.track.speed, 1.0);
    assert!(snapshot.track.position < Duration::from_secs(3));
    let mark = slot.mark.expect("consumed source mark");
    assert_eq!(snapshot.track.mark, Some(mark));
    let host = rig.host.session_grid().await;
    let BeatGridQuery::Resolved(beat) = host.beat_at(MapPoint::new(
        host.stamp(),
        MapPosition::Session(mark.session),
    )) else {
        panic!("the Host resolves the current source frame");
    };
    let phase = (snapshot.track.position.as_secs_f64() * 2.0 - f64::from(*beat.value().value()))
        .rem_euclid(1.0);
    assert!(
        phase.min(1.0 - phase) <= 4.0 / f64::from(case.sample_rate),
        "the old source programme has no phase catch-up"
    );
    rig.decks[0].observe(|observation| {
        let speed = observation
            .speeds
            .last()
            .expect("accepted source programme");
        let When::At(start) = speed.at else {
            panic!("timed source programme");
        };
        let elapsed = mark
            .session
            .frames_since(start)
            .expect("source programme already started");
        let SpeedCurve::ApproachSteps(laws) = &speed.curve else {
            panic!(
                "the source programme must retain its changing future law: {:?}",
                speed.curve
            );
        };
        assert!(
            laws.iter()
                .any(|(at, law)| *at > elapsed && law.target() == 2.0),
            "the old programme still owes its faster middle region"
        );
        assert_eq!(laws.last().expect("programme tail").1.target(), 1.0);
        assert!(
            observation
                .placement_receipts
                .iter()
                .any(|(owner, origin, receipt)| *owner == item
                    && *origin == LinkReceiptOrigin::Lane
                    && matches!(receipt,
                kithara::play::Settled::Applied { seq, .. } if *seq == speed.seq)),
            "the nonconstant programme has an actual Applied lane receipt"
        );
        (observation.speeds.len(), observation.placements.len())
    })
}
