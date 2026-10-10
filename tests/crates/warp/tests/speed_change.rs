//! A deck that changes speed mid-play, key-locked or varispeed: every source
//! beat sounds once at the new step, the ring the device reads never runs dry
//! and the source opens once.

use std::num::NonZeroU32;

use kithara::{
    host::{
        HostConfig, HostOwned, HostSettings, HostSettingsControl, MetronomeConfig,
        MetronomeConfigControl, Tap,
    },
    platform::time::{self, Duration, Instant},
    play::{ResourceConfig, ResourceSrc},
    queue::{Queue, QueueConfig, Transition},
    warp::{StretchKind, WarpConfig},
};
use kithara_integration_tests::{
    audio_artifact::{AudioArtifactTap, artifact_label},
    bufpool_ext::{TestPools, pools},
    disk_asset_store, kithara,
    offline::{OfflinePlayer, OfflinePlayerOptions},
    usdt_trace,
    waits::wait_for_loader_done_event,
};
use kithara_test_fixtures::assets::rhythm_wav_deck_b_120bpm_48k;
use kithara_test_utils::{TestTempDir, temp_dir};
use num_traits::ToPrimitive;

const SAMPLE_RATE: u32 = 48_000;
const CHANNELS: usize = 2;
const BLOCK_FRAMES: usize = 512;
/// Source frames between two beats of the 120 BPM pulse track.
const BEAT_FRAMES: f64 = 24_000.0;
/// Blocks before the capture counts underruns: the ring fills after the start.
const WARMUP_BLOCKS: usize = 48;
/// Blocks the deck plays.
const TOTAL_BLOCKS: usize = 844;
/// The deck's first speed: the renderer passes the source through at unity.
const START_SPEED: f32 = 1.0;
/// Each change: the block it is sent before and the speed it sets. The first
/// leaves unity, the second changes a speed already off it.
const CHANGES: [(usize, f32); 2] = [(188, 1.07), (469, 0.94)];
/// A beat the pulse's onset crosses: a fifth of the quieter beat's peak.
const ONSET_LEVEL: f32 = 0.06;
/// Frames after an onset no other onset starts: well past a 40 ms pulse and
/// well short of the shortest beat.
const REFRACTORY_FRAMES: usize = 12_000;
/// Frames of a pulse its level is read over: its 40 ms.
const PULSE_FRAMES: usize = 1_920;
/// A downbeat pulse sounds 1.4 times louder than a beat.
const DOWNBEAT_RATIO: f32 = 1.2;
/// Onsets on each side a pulse is weighed against: any five consecutive
/// onsets hold at most two downbeats, so their median is a beat.
const NEIGHBOURS: usize = 2;
const BEATS_PER_BAR: usize = 4;
/// Output frames a change may take to reach the device: the ring the worker
/// renders ahead plus one block.
const CHANGE_REACH_FRAMES: usize = 48_000;
/// Slack on an onset interval: the engine places a smeared attack within a
/// few milliseconds.
const INTERVAL_TOLERANCE: f64 = 480.0;
/// Device level of the Host metronome over the deck, undamped.
const METRONOME_LEVEL: f32 = 0.5;
const NO_DUCK: f32 = 0.0;

struct Onset {
    frame: usize,
    downbeat: bool,
}

struct Take {
    master: Vec<f32>,
    underruns: u64,
    /// Sources the deck opened from the load to its last block.
    opened: usize,
}

fn session() -> HostConfig<TestPools> {
    HostConfig::offline(pools())
        .max_block_frames(
            NonZeroU32::new(u32::try_from(BLOCK_FRAMES).expect("block fits u32"))
                .expect("block is non-zero"),
        )
        .settings(
            HostSettings::builder()
                .sample_rate(NonZeroU32::new(SAMPLE_RATE).expect("sample rate is non-zero"))
                .metronome(
                    MetronomeConfig::builder()
                        .level(METRONOME_LEVEL)
                        .duck(NO_DUCK)
                        .build(),
                )
                .build(),
        )
        .build()
}

async fn playing_deck(
    temp_dir: &TestTempDir,
    backend: StretchKind,
    keylock: bool,
) -> (OfflinePlayer, HostOwned<Queue<TestPools>>) {
    let harness = OfflinePlayer::with_options(
        OfflinePlayerOptions::builder()
            .crossfade_duration(0.0)
            .warp(
                WarpConfig::builder()
                    .speed(START_SPEED)
                    .backend(backend)
                    .keylock(keylock)
                    .build(),
            )
            .build(),
        session(),
    )
    .await;
    let queue = Queue::new(
        QueueConfig::builder()
            .prep(harness.resource_prep().clone())
            .build(),
    );
    let queue = harness.insert(queue).await;
    harness
        .run(queue.control(), |q| q.set_default_rate(START_SPEED))
        .await
        .expect("a finite rate is accepted");
    let path = rhythm_wav_deck_b_120bpm_48k()
        .path()
        .expect("the pulse track lives on disk");
    let config: ResourceConfig<TestPools> = ResourceConfig::for_src(
        ResourceSrc::parse(path.to_str().expect("utf-8 fixture path"))
            .expect("fixture path is a valid resource source"),
    )
    .store(disk_asset_store(temp_dir.path().join("speed-store")))
    .build();
    let mut events = queue.subscribe();
    let id = harness
        .run(queue.control(), move |q| q.append(config))
        .await
        .expect("append the pulse track");
    wait_for_loader_done_event(&mut events, &queue, id, Duration::from_secs(30))
        .await
        .expect("load the pulse track");
    harness
        .run(queue.control(), move |q| q.select(id, Transition::None))
        .await
        .expect("select the pulse track");
    (harness, queue)
}

/// Plays the deck at the device cadence and sends each change before its
/// block. The pacing sleep shares the clock of the worker's park, so a block
/// period elapses only once the worker has rendered; an underrun is then the
/// lane's, not the machine's.
#[kithara::flash(true)]
async fn play(temp_dir: &TestTempDir, backend: StretchKind, keylock: bool) -> Take {
    let trace = usdt_trace::scope();
    let (harness, queue) = playing_deck(temp_dir, backend, keylock).await;
    let mut master = harness
        .host()
        .attach_tap(Tap::Master, TOTAL_BLOCKS * BLOCK_FRAMES * CHANNELS)
        .await
        .expect("master tap");
    harness
        .host()
        .with(|host| host.metronome().set_enabled(true))
        .await
        .expect("metronome on");
    let mut recording = AudioArtifactTap::from_env(
        &artifact_label(),
        SAMPLE_RATE,
        u16::try_from(CHANNELS).expect("channels fit u16"),
    )
    .expect("open the listening artifact");
    let period = Duration::from_secs_f64(
        BLOCK_FRAMES.to_f64().expect("block fits f64") / f64::from(SAMPLE_RATE),
    );
    let mut warmed = None;
    for block in 0..TOTAL_BLOCKS {
        if let Some((_, speed)) = CHANGES.iter().find(|(at, _)| *at == block) {
            let speed = *speed;
            harness
                .run(queue.control(), move |q| q.set_rate(speed))
                .await
                .expect("a finite rate is accepted");
            if let Some(recording) = recording.as_mut() {
                recording.mark(&format!("speed {speed}"));
            }
        }
        if block == WARMUP_BLOCKS {
            warmed = Some(harness.metrics().underruns());
        }
        let started = Instant::now();
        let output = harness.render(BLOCK_FRAMES).await;
        if let Some(recording) = recording.as_mut() {
            recording.push(&output);
        }
        time::sleep(period.saturating_sub(started.elapsed())).await;
    }
    let opened = trace.events_of("source_opened").len();
    drop(trace);
    let underruns = harness
        .metrics()
        .underruns()
        .saturating_sub(warmed.expect("the capture outlasts the warmup"));
    assert_eq!(master.drops(), 0, "the master tap keeps every frame");
    let master = master.drain();
    drop(queue);
    harness.close().await;
    Take {
        master,
        underruns,
        opened,
    }
}

fn onsets(master: &[f32]) -> Vec<Onset> {
    let left: Vec<f32> = master
        .chunks_exact(CHANNELS)
        .map(|frame| frame[0])
        .collect();
    let mut found = Vec::new();
    let mut frame = 0;
    while frame < left.len() {
        if left[frame].abs() < ONSET_LEVEL {
            frame += 1;
            continue;
        }
        let pulse = &left[frame..(frame + PULSE_FRAMES).min(left.len())];
        let energy = pulse.iter().map(|sample| sample * sample).sum::<f32>();
        let rms = (energy / pulse.len().to_f32().expect("pulse fits f32")).sqrt();
        found.push((frame, rms));
        frame += REFRACTORY_FRAMES;
    }
    (0..found.len())
        .map(|index| {
            let mut neighbours: Vec<f32> = found
                [index.saturating_sub(NEIGHBOURS)..(index + NEIGHBOURS + 1).min(found.len())]
                .iter()
                .map(|(_, rms)| *rms)
                .collect();
            neighbours.sort_by(f32::total_cmp);
            let beat = neighbours[neighbours.len() / 2];
            Onset {
                frame: found[index].0,
                downbeat: found[index].1 > beat * DOWNBEAT_RATIO,
            }
        })
        .collect()
}

fn step(speed: f32) -> f64 {
    BEAT_FRAMES / f64::from(speed)
}

/// The speed in force at output `frame`, unless a change may still be on its
/// way to the device there.
fn settled_speed(frame: usize) -> Option<f32> {
    let mut speed = START_SPEED;
    for (block, next) in CHANGES {
        let sent = block * BLOCK_FRAMES;
        if frame >= sent + CHANGE_REACH_FRAMES {
            speed = next;
        } else if frame >= sent {
            return None;
        }
    }
    Some(speed)
}

/// Every way the onsets break the beat: an interval between two settled
/// onsets off the step of their speed, an interval across a change outside
/// the steps it joins, and a downbeat off the bar.
fn beat_failures(onsets: &[Onset]) -> Vec<String> {
    let mut failures = Vec::new();
    let mut speeds = vec![START_SPEED];
    speeds.extend(CHANGES.iter().map(|(_, speed)| *speed));
    let shortest = speeds.iter().copied().map(step).fold(f64::MAX, f64::min);
    let longest = speeds.iter().copied().map(step).fold(0.0, f64::max);
    for pair in onsets.windows(2) {
        let interval = (pair[1].frame - pair[0].frame)
            .to_f64()
            .expect("interval fits f64");
        match (settled_speed(pair[0].frame), settled_speed(pair[1].frame)) {
            (Some(from), Some(to)) if from == to => {
                if (interval - step(from)).abs() > INTERVAL_TOLERANCE {
                    failures.push(format!(
                        "onsets at {} and {}: {interval} frames apart, the step at {from} is {:.0}",
                        pair[0].frame,
                        pair[1].frame,
                        step(from),
                    ));
                }
            }
            _ => {
                if interval < shortest - INTERVAL_TOLERANCE
                    || interval > longest + INTERVAL_TOLERANCE
                {
                    failures.push(format!(
                        "onsets at {} and {} across a change: {interval} frames apart, outside {shortest:.0}..{longest:.0}",
                        pair[0].frame, pair[1].frame,
                    ));
                }
            }
        }
    }
    let first_downbeat = onsets.iter().position(|onset| onset.downbeat);
    match first_downbeat {
        None => failures.push("no downbeat sounded".to_owned()),
        Some(first) => {
            for (index, onset) in onsets.iter().enumerate() {
                let on_bar = index >= first && (index - first) % BEATS_PER_BAR == 0;
                if onset.downbeat != on_bar {
                    failures.push(format!(
                        "onset {index} at frame {} {} a downbeat off the bar",
                        onset.frame,
                        if onset.downbeat { "is" } else { "is not" },
                    ));
                }
            }
        }
    }
    failures
}

#[kithara::test(
    tokio,
    serial,
    timeout(Duration::from_secs(120)),
    hang_timeout_secs(10)
)]
#[case::signalsmith(StretchKind::Signalsmith, true)]
#[cfg_attr(
    all(
        not(target_os = "android"),
        not(all(target_os = "windows", target_env = "msvc"))
    ),
    case::bungee(StretchKind::Bungee, true)
)]
#[case::varispeed(StretchKind::Signalsmith, false)]
async fn a_speed_change_keeps_every_beat_on_the_source_it_opened(
    temp_dir: TestTempDir,
    #[case] backend: StretchKind,
    #[case] keylock: bool,
) {
    let take = play(&temp_dir, backend, keylock).await;
    let onsets = onsets(&take.master);
    let deck = if keylock {
        format!("{backend} keylock")
    } else {
        format!("{backend} varispeed")
    };

    assert_eq!(take.opened, 1, "{deck}: a speed change opens no source");
    assert_eq!(take.underruns, 0, "{deck}: the ring ran dry");
    let failures = beat_failures(&onsets);
    let frames: Vec<usize> = onsets.iter().map(|onset| onset.frame).collect();
    assert!(
        failures.is_empty(),
        "{deck}: the beat broke:\n{}\nonsets: {frames:?}",
        failures.join("\n"),
    );
    let last_change = CHANGES[CHANGES.len() - 1].0 * BLOCK_FRAMES + CHANGE_REACH_FRAMES;
    assert!(
        onsets
            .iter()
            .filter(|onset| onset.frame >= last_change)
            .count()
            >= BEATS_PER_BAR,
        "{deck}: a bar sounds at the last speed: onsets {frames:?}",
    );
}
