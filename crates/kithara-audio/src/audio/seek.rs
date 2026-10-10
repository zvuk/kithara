use std::num::NonZeroU32;

use kithara_decode::TrackMetadata;
use kithara_events::{DeferredBus, EventBus};
use kithara_platform::{CancelScope, sync::Arc, time::Duration};
use kithara_signal::{AudioChunk, AudioChunkInfo, AudioSpec};
use kithara_stream::{ActivityWriter, PlayheadState, PlayheadWrite};
use kithara_test_utils::kithara;

use super::{Audio, core::AudioContext};
use crate::{
    AudioSource, ChunkOutcome, Fetch, SeekOutcome, TrackStep,
    test_pools::{pools, sample_buffer},
};

struct LandingSource {
    playhead: Arc<PlayheadState>,
    spec: AudioSpec,
    target: Duration,
    delivered: bool,
}

impl AudioSource for LandingSource {
    type Chunk = AudioChunk;

    fn seek(&mut self, target: Duration) -> Result<SeekOutcome, crate::AudioReadError> {
        self.target = target;
        self.delivered = false;
        self.playhead.set_position(target);
        Ok(SeekOutcome::Landed {
            target,
            landed_at: target,
        })
    }

    fn set_host_sample_rate(&mut self, rate: NonZeroU32) {
        self.spec.sample_rate = rate;
    }

    fn host_sample_rate(&self) -> Option<NonZeroU32> {
        Some(self.spec.sample_rate)
    }

    fn step_track(&mut self) -> TrackStep<AudioChunk> {
        if self.delivered {
            return TrackStep::Eof;
        }
        self.delivered = true;
        let frame_offset = self.target.as_secs() * u64::from(self.spec.sample_rate.get());
        TrackStep::Produced(Fetch::data(AudioChunk::new(
            AudioChunkInfo {
                spec: self.spec,
                frames: 4,
                frame_offset,
                timestamp: self.target,
                end_timestamp: self.target + Duration::from_millis(4),
                ..AudioChunkInfo::default()
            },
            sample_buffer(&pools(), &[self.target.as_secs_f32(); 4]),
        )))
    }
}

fn audio() -> Audio<()> {
    let writer = ActivityWriter::new();
    let playhead = Arc::new(PlayheadState::new());
    let spec = AudioSpec::new(1, NonZeroU32::new(1_000).expect("test rate"));
    Audio::new(
        Box::new(LandingSource {
            playhead: Arc::clone(&playhead),
            spec,
            target: Duration::ZERO,
            delivered: false,
        }),
        AudioContext {
            playhead,
            emit: Arc::new(DeferredBus::new(
                EventBus::default(),
                crate::consts::AUDIO_EVENT_CAPACITY,
            )),
            metadata: TrackMetadata::default(),
            abr: None,
            activity: writer.reader(),
            activity_writer: Some(writer),
            cancel: CancelScope::new(None).token(),
        },
        spec,
    )
}

fn landing(audio: &mut Audio<()>) -> AudioChunk {
    match audio.next_chunk().expect("landing read") {
        ChunkOutcome::Chunk(chunk) => *chunk,
        outcome => panic!("expected landing PCM, got {outcome:?}"),
    }
}

#[kithara::test]
fn stale_complete_leaves_newer_seek_intact() {
    let mut audio = audio();
    audio.seek(Duration::from_secs(5)).expect("first seek");
    audio.preload().expect("stage first landing");
    audio.seek(Duration::from_secs(10)).expect("second seek");
    let chunk = landing(&mut audio);
    assert!(
        chunk.samples.iter().all(|sample| *sample == 10.0),
        "newer seek PCM must survive the older landing"
    );
    assert_eq!(chunk.meta.timestamp, Duration::from_secs(10));
}

#[kithara::test]
fn complete_seek_ignores_stale_epoch() {
    let mut audio = audio();
    audio.seek(Duration::from_secs(5)).expect("first seek");
    audio.preload().expect("stage first landing");
    audio.seek(Duration::from_secs(10)).expect("second seek");
    let chunk = landing(&mut audio);
    assert!(chunk.samples.iter().all(|sample| *sample == 10.0));
    assert_eq!(chunk.meta.timestamp, Duration::from_secs(10));
    assert!(matches!(
        audio.next_chunk().expect("after current landing"),
        ChunkOutcome::Eof { .. }
    ));
}

#[kithara::test]
fn complete_seek_does_not_clobber_concurrent_target() {
    let mut audio = audio();
    audio.seek(Duration::from_secs(5)).expect("first seek");
    audio.preload().expect("stage first landing");
    audio.seek(Duration::from_secs(15)).expect("second seek");
    let chunk = landing(&mut audio);
    assert!(chunk.samples.iter().all(|sample| *sample == 15.0));
    assert_eq!(chunk.meta.timestamp, Duration::from_secs(15));
}

#[kithara::test]
fn initiate_seek_does_not_touch_playing() {
    let mut audio = audio();
    let mut writer = audio.take_activity_writer().expect("sole writer");
    writer.set_playing(true);
    let activity = audio.activity();
    audio.seek(Duration::from_secs(5)).expect("seek");
    assert!(
        activity.is_playing(),
        "PLAYING must not be affected by seek"
    );
    audio.preload().expect("prepare landing");
    assert!(
        activity.is_playing(),
        "PLAYING must survive seek preparation"
    );
    landing(&mut audio);
    assert!(
        activity.is_playing(),
        "PLAYING must survive output acknowledgement"
    );
}
