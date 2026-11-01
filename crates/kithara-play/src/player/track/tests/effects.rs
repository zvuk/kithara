use kithara_audio::{AudioSource, Fetch, TrackStep};
use kithara_bufpool::PoolRegion;
use kithara_effects::{
    AudioEffect, EffectChange, EffectConfig, EffectTarget, GainDb,
    eq::{EqBandConfig, EqBandConfigChange, EqConfig, EqEffect},
};
use kithara_signal::AudioChunk;
use kithara_test_utils::bufpool::sample_buffer;

use super::*;
use crate::EffectSnapshot;

const FRAMES: u32 = 96;
const AT: u64 = 17;

fn input(region: &PoolRegion<TestPools>, frames: u32) -> AudioChunk {
    let spec = AudioSpec::new(2, mock::SAMPLE_RATE);
    AudioChunk::new(
        AudioChunkInfo {
            spec,
            frames,
            end_timestamp: spec.duration_for(u64::from(frames)).unwrap(),
            ..AudioChunkInfo::default()
        },
        sample_buffer(region, &vec![0.25; frames as usize * 2]),
    )
}

async fn configured_track(rig: &mut Rig) -> (Track, Inbox<LaneProtocol>) {
    let resource =
        ResourceConfig::for_src(ResourceSrc::parse("https://example.com/effect.wav").unwrap())
            .store(AssetStore::builder(pools()).build())
            .worker(crate::PlayWorker::new(
                crate::PlayWorkerConfig::builder(pools()).build(),
            ))
            .host_sample_rate(mock::SAMPLE_RATE)
            .effects(vec![EffectConfig::Equalizer(vec![EqBandConfig::default()])])
            .build();
    let item = ResourceLoad::new(resource, Box::new(AudioObserverSlot::default().relay()));
    loaded_with_item(rig, A, "configured EQ", item).await
}

fn effect_source(
    inbox: Inbox<LaneProtocol>,
    region: &PoolRegion<TestPools>,
) -> kithara_render::WarpSource<kithara_render::mock::ScriptedSource, TestPools> {
    let spec = AudioSpec::new(2, mock::SAMPLE_RATE);
    let eq = EqEffect::new(
        &EqConfig::builder(region.clone()).build(),
        vec![EqBandConfig::default()],
        spec.sample_rate.get(),
        spec.channels,
    )
    .unwrap();
    let raw = kithara_render::mock::ScriptedSource::new([TrackStep::Produced(Fetch::data(input(
        region, FRAMES,
    )))]);
    let warp = kithara_warp::Warp::new(
        (),
        &kithara_warp::WarpConfig::builder()
            .render_quantum_frames(std::num::NonZeroUsize::new(13).unwrap())
            .build(),
    );
    kithara_render::WarpSource::new(
        raw,
        warp.renderer(spec, region.clone()),
        vec![Box::new(eq)],
        kithara_effects::EffectDrain::new(1, region).unwrap(),
        spec,
        region.clone(),
        kithara_render::LaneSetup {
            inbox,
            preload_chunks: std::num::NonZeroUsize::MIN,
            declick: kithara_dsp::param::SmootherConfig::default(),
        },
    )
}

fn confirmed_gain(track: &Track) -> GainDb {
    let snapshot = track.snapshot();
    let EffectSnapshot::Equalizer { bands, .. } = &snapshot.effects[0];
    bands[0].config().gain_db()
}

#[kithara::test]
async fn slot_effect_parameters_execute_on_audio_time() {
    let region = pools();
    let mut rig = rig();
    let (mut track, inbox) = configured_track(&mut rig).await;
    let lane = track.lane_id.unwrap();
    let anchor = i64::try_from(track.ring_depth.get() + track.engine_latency.get()).unwrap() + 4096;
    track.status = TrackStatus::Playing {
        since: frame(anchor),
    };
    track.mark = Some(SlotMark {
        session: frame(anchor),
        lane: LaneFrame::default(),
        position: Duration::ZERO,
    });
    command_at_zero(
        &mut track,
        &mut rig,
        TrackCommand::ConfigureEffect {
            lane,
            target: EffectTarget::EqBand { effect: 0, band: 0 },
            change: EffectChange::EqBand(EqBandConfigChange::GainDb(GainDb::MIN)),
            at: When::At(frame(anchor + i64::try_from(AT).unwrap())),
        },
    )
    .expect("one admitted effect step");
    assert_eq!(confirmed_gain(&track), GainDb::DEFAULT);

    let mut expected = EqEffect::new(
        &EqConfig::builder(region.clone()).build(),
        vec![EqBandConfig::default()],
        mock::SAMPLE_RATE.get(),
        2,
    )
    .unwrap();
    let prefix = expected
        .process(input(&region, u32::try_from(AT).unwrap()))
        .unwrap();
    expected.set_gain(0, GainDb::MIN);
    let suffix = expected
        .process(input(&region, FRAMES - u32::try_from(AT).unwrap()))
        .unwrap();
    let expected: Vec<_> = prefix
        .samples
        .iter()
        .chain(suffix.samples.iter())
        .copied()
        .collect();
    let mut source = effect_source(inbox, &region);
    let mut rendered = Vec::new();
    let mut saw_boundary = false;
    for _ in 0..32 {
        let _ = source.prepare_deferred();
        source.finish_deferred();
        match source.step_track() {
            TrackStep::Produced(Fetch::Data { data, .. }) => {
                let start = data.meta.lane_frame;
                assert!(start >= AT || start + u64::from(data.meta.frames) <= AT);
                saw_boundary |= start == AT;
                source.admitted();
                rendered.extend_from_slice(&data.samples);
                in_pass(&mut rig, frame(0), |out| track.tick(frame(0), out));
                assert_eq!(
                    confirmed_gain(&track),
                    if start < AT {
                        GainDb::DEFAULT
                    } else {
                        GainDb::MIN
                    }
                );
            }
            TrackStep::StateChanged => {}
            TrackStep::Eof => break,
            _ => panic!("the prepared EQ lane must produce finite PCM"),
        }
    }
    assert!(saw_boundary);
    assert_eq!(rendered.len(), FRAMES as usize * 2);
    assert_eq!(
        rendered.iter().map(|s| s.to_bits()).collect::<Vec<_>>(),
        expected.iter().map(|s| s.to_bits()).collect::<Vec<_>>()
    );
}

#[kithara::test]
async fn slot_effect_stale_or_invalid_parameters_are_atomic() {
    let mut rig = rig();
    let (mut track, inbox) = configured_track(&mut rig).await;
    let (other, _) = loaded(&mut rig, B, "other identity").await;
    let lane = track.lane_id.unwrap();
    let stale = other.lane_id.unwrap();
    assert_ne!(lane, stale);
    let room = track.lane.as_ref().unwrap().available();
    for target in [
        EffectTarget::EqBand { effect: 9, band: 0 },
        EffectTarget::EqBand { effect: 0, band: 9 },
    ] {
        assert!(matches!(
            in_pass(&mut rig, frame(0), |out| track.apply(
                TrackCommand::ConfigureEffect {
                    lane,
                    target,
                    change: EffectChange::EqBand(EqBandConfigChange::GainDb(GainDb::MIN)),
                    at: When::Next,
                },
                out
            )),
            Err(PlayError::InvalidConfiguration { .. })
        ));
    }
    assert!(matches!(
        in_pass(&mut rig, frame(0), |out| track.apply(
            TrackCommand::ConfigureEffect {
                lane: stale,
                target: EffectTarget::EqBand { effect: 0, band: 0 },
                change: EffectChange::EqBand(EqBandConfigChange::GainDb(GainDb::MIN)),
                at: When::Next,
            },
            out
        )),
        Err(PlayError::NotReady)
    ));
    assert_eq!(track.lane.as_ref().unwrap().available(), room);
    assert_eq!(confirmed_gain(&track), GainDb::DEFAULT);

    track
        .send_lane_batch(
            vec![
                LaneCommand::SetSpeed(SpeedCurve::Constant(1.5)),
                LaneCommand::ConfigureEffect {
                    target: EffectTarget::EqBand { effect: 0, band: 0 },
                    change: EffectChange::EqBand(EqBandConfigChange::GainDb(GainDb::MIN)),
                },
            ],
            When::Next,
        )
        .unwrap();
    track
        .send_lane(
            LaneCommand::ConfigureEffect {
                target: EffectTarget::EqBand {
                    effect: 0,
                    band: 99,
                },
                change: EffectChange::EqBand(EqBandConfigChange::GainDb(GainDb::MIN)),
            },
            When::Next,
        )
        .unwrap();
    let region = pools();
    let mut source = effect_source(inbox, &region);
    let mut rendered = Vec::new();
    for _ in 0..32 {
        let _ = source.prepare_deferred();
        source.finish_deferred();
        match source.step_track() {
            TrackStep::Produced(Fetch::Data { data, .. }) => {
                rendered.extend_from_slice(&data.samples);
            }
            TrackStep::StateChanged => {}
            TrackStep::Eof => break,
            _ => panic!("an invalid effect step must not terminate playback"),
        }
    }
    in_pass(&mut rig, frame(0), |out| track.tick(frame(0), out));
    assert_eq!(rendered, vec![0.25; FRAMES as usize * 2]);
    assert_eq!(confirmed_gain(&track), GainDb::DEFAULT);
    assert_eq!(
        track
            .lane_commands
            .iter()
            .filter(|operation| operation.applied() == Some(false))
            .count(),
        3
    );
}
