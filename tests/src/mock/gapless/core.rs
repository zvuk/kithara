#![cfg(not(target_arch = "wasm32"))]

pub use std::num::{NonZeroU32, NonZeroUsize};

pub use kithara::{
    audio::{
        AudioControl, AudioRead, AudioReadError, AudioSession, ChunkOutcome, ReadOutcome,
        SeekOutcome,
    },
    decode::{GaplessInfo, GaplessMode, GaplessTailCompensation, GaplessTrimmer, TrackMetadata},
    events::{EventBus, TrackId},
    platform::time::{self, Duration},
    play::{PlayerEvent, ResourceConfig, ResourceSrc},
    queue::{TrackSource, Transition},
    signal::{AudioChunk, AudioChunkInfo, AudioSpec},
    stream::AudioCodec,
};
#[cfg(all(
    feature = "apple-fused-src",
    any(target_os = "macos", target_os = "ios")
))]
pub use kithara::{
    audio::{AudioDecoderConfig, DecoderResamplerSettings},
    decode::DecoderBackend,
    platform::time::WallInstant,
    play::{PlayWorker, PlayWorkerConfig, PlaybackResamplerBackend, Resource},
};
pub use kithara_test_fixtures::{
    analysis_beat_fixtures::{fused_seam, fused_seam_stereo},
    signal::{deinterleave_left, goertzel_magnitude, max_silence_run},
};
use kithara_test_utils::kithara as watchdog;
pub use kithara_test_utils::{
    TestTempDir,
    bufpool::{TestPools, pools},
    temp_dir,
};

use super::consts::*;
pub use crate::{
    CreatedHls, HlsFixtureBuilder, TestServerHelper,
    fixture_protocol::{
        GaplessEncoding, PackagedAudioRequest, PackagedAudioSource, PackagedSignal,
    },
    gapless::{
        AAC_FRAME_SAMPLES, AAC_GAPLESS_ENCODER_DELAY, AAC_GAPLESS_SEGMENT_FRAMES,
        AAC_GAPLESS_SEGMENT_SECS, AAC_GAPLESS_SEGMENTS, AAC_GAPLESS_TRAILING_DELAY,
        GAPLESS_CHANNELS, GAPLESS_SAMPLE_RATE,
    },
    offline::{OfflinePlayer, OfflinePlayerOptions, TimedPlayerEvent},
};

pub fn expected_visible_frames(encoder_delay: u32, trailing_delay: u32) -> usize {
    let packets_per_segment = AAC_GAPLESS_SEGMENT_FRAMES.div_ceil(AAC_FRAME_SAMPLES);
    let native_encoder_delay = AAC_FRAME_SAMPLES;
    packets_per_segment
        .saturating_mul(AAC_FRAME_SAMPLES)
        .saturating_mul(AAC_GAPLESS_SEGMENTS)
        .saturating_sub(native_encoder_delay)
        .saturating_sub(usize::try_from(encoder_delay).expect("encoder delay fits usize"))
        .saturating_sub(usize::try_from(trailing_delay).expect("trailing delay fits usize"))
}

pub fn expected_total_decoded_frames() -> usize {
    let packets_per_segment = AAC_GAPLESS_SEGMENT_FRAMES.div_ceil(AAC_FRAME_SAMPLES);
    let native_encoder_delay = AAC_FRAME_SAMPLES;
    packets_per_segment
        .saturating_mul(AAC_FRAME_SAMPLES)
        .saturating_mul(AAC_GAPLESS_SEGMENTS)
        .saturating_sub(native_encoder_delay)
}

pub async fn gapless_source(
    encoder_delay: Option<u32>,
    trailing_delay: Option<u32>,
    start_frame: u64,
    gapless_encoding: GaplessEncoding,
) -> (TestServerHelper, CreatedHls) {
    let server = TestServerHelper::new().await;
    let created = server
        .create_hls(
            HlsFixtureBuilder::new()
                .variant_count(1)
                .segments_per_variant(AAC_GAPLESS_SEGMENTS)
                .segment_duration_secs(AAC_GAPLESS_SEGMENT_SECS)
                .packaged_audio(PackagedAudioRequest {
                    codec: AudioCodec::AacLc,
                    sample_rate: GAPLESS_SAMPLE_RATE,
                    channels: GAPLESS_CHANNELS,
                    start_frame: NonZeroU32::new(
                        u32::try_from(start_frame).expect("start_frame fits u32"),
                    ),
                    timescale: Some(GAPLESS_SAMPLE_RATE),
                    bit_rate: Some(128_000),
                    encoder_delay: encoder_delay.and_then(NonZeroU32::new),
                    trailing_delay: trailing_delay.and_then(NonZeroU32::new),
                    source: PackagedAudioSource::Signal(PackagedSignal::Sine { freq_hz: SINE_HZ }),
                    gapless_encoding,
                    variant_overrides: Vec::new(),
                }),
        )
        .await
        .expect("create gapless e2e HLS fixture");

    (server, created)
}

pub async fn create_resource(
    _harness: &OfflinePlayer,
    created: &CreatedHls,
    cache_dir: &std::path::Path,
) -> ResourceConfig<TestPools> {
    ResourceConfig::<TestPools>::for_src(
        ResourceSrc::parse(created.master_url().as_str()).expect("valid HLS master URL"),
    )
    .store(crate::disk_asset_store(cache_dir))
    .build()
}

#[cfg(all(
    feature = "apple-fused-src",
    any(target_os = "macos", target_os = "ios")
))]
pub async fn create_apple_fused_resource(
    worker: &PlayWorker<TestPools>,
    created: &CreatedHls,
    cache_dir: &std::path::Path,
) -> ResourceConfig<TestPools> {
    let store = crate::disk_asset_store(cache_dir);
    let decoder_defaults = AudioDecoderConfig::builder()
        .resampler(
            DecoderResamplerSettings::builder()
                .backend(PlaybackResamplerBackend::default())
                .build(),
        )
        .build();
    ResourceConfig::for_src(
        ResourceSrc::parse(created.master_url().as_str()).expect("valid HLS master URL"),
    )
    .store(store)
    .worker(worker.clone())
    .host_sample_rate(NonZeroU32::new(FUSED_FIXTURE_DEVICE_RATE).expect("device rate"))
    .block_on_underrun(true)
    .decoder(
        AudioDecoderConfig::builder()
            .backend(DecoderBackend::Apple)
            .gapless_mode(GaplessMode::MediaOnly)
            .maybe_resampler(decoder_defaults.resampler().cloned())
            .build(),
    )
    .build()
}

pub async fn render_synthetic_fused_deficit_seam(
    tail_compensation: bool,
    stereo: &[f32],
) -> SyntheticSeamRender {
    let expected_device_frames = ceil_scaled_frames(
        FUSED_FIXTURE_SOURCE_FRAMES,
        FUSED_FIXTURE_DEVICE_RATE,
        FUSED_FIXTURE_SOURCE_RATE,
    );
    assert_eq!(
        expected_device_frames,
        u64::try_from(FUSED_FIXTURE_IDEAL_DEVICE_FRAMES).expect("fixture size fits u64")
    );

    let harness = OfflinePlayer::with_sample_rate(
        OfflinePlayerOptions::builder()
            .block_on_underrun(true)
            .crossfade_duration(0.0)
            .gapless(true)
            .gapless_mode(GaplessMode::Disabled)
            .build(),
        FUSED_FIXTURE_DEVICE_RATE,
    )
    .await;
    harness
        .with_queue(move |queue| queue.set_level(FUSED_FIXTURE_MASTER_LEVEL))
        .await
        .expect("set the mix level");
    let first_frames = synthetic_tail_trimmed_first_frames(tail_compensation, stereo);
    let first_frame_count = first_frames.len();
    let first = harness.pcm_deck(Box::new(SyntheticPcmReader::new(
        first_frames,
        first_frame_count,
    )));
    let second = TrackSource::Uri(
        kithara_test_fixtures::assets::fused_seam_second_wav_default()
            .path()
            .expect("prepared fused seam second source")
            .to_str()
            .expect("UTF-8 prepared fused seam path")
            .to_owned(),
    );

    let [_, second_id] = load_tagged_queue(&harness, [first, second]).await;

    let (rendered, events) = render_until_item_end(&harness, second_id).await;
    let left = deinterleave_left(&rendered, usize::from(GAPLESS_CHANNELS));
    let peak = left.iter().map(|sample| sample.abs()).fold(0.0, f32::max);
    assert!(
        (peak - FUSED_FIXTURE_MASTER_LEVEL).abs() <= 1.0e-3,
        "fused seam fixture must preserve its explicit headroom; peak={peak}"
    );
    let result = SyntheticSeamRender {
        left,
        events,
        first_frames: first_frame_count,
    };
    harness.close().await;
    result
}

pub fn synthetic_tail_trimmed_first_frames(tail_compensation: bool, stereo: &[f32]) -> Vec<f32> {
    let actual_pre_trim_frames =
        FUSED_FIXTURE_IDEAL_DEVICE_FRAMES + usize::try_from(TRAILING_FRAMES).expect("fits") - 1;
    let ideal_pre_trim_frames =
        u64::try_from(actual_pre_trim_frames).expect("fixture frame count fits u64") + 1;
    let source = synthetic_interleaved_chunk(
        &stereo[..actual_pre_trim_frames * usize::from(GAPLESS_CHANNELS)],
    );
    let mut trimmer = GaplessTrimmer::from(GaplessInfo::new(0, TRAILING_FRAMES))
        .with_tail_compensation(
            tail_compensation.then_some(GaplessTailCompensation::new(ideal_pre_trim_frames)),
        );

    let mut output = Vec::new();
    output.extend(left_frames_from_chunks(
        trimmer.push(source).expect("valid gapless PCM"),
    ));
    output.extend(left_frames_from_chunks(
        trimmer.flush().expect("valid gapless PCM"),
    ));
    output
}

pub fn synthetic_interleaved_chunk(pcm: &[f32]) -> AudioChunk {
    let spec = AudioSpec::new(
        GAPLESS_CHANNELS,
        NonZeroU32::new(FUSED_FIXTURE_DEVICE_RATE).expect("test sample rate"),
    );
    let frame_count = pcm.len() / usize::from(GAPLESS_CHANNELS);
    let sample_count = frame_count * usize::from(GAPLESS_CHANNELS);
    let mut samples = pools()
        .get_with_len::<f32>(sample_count)
        .expect("synthetic chunk exceeds PCM pool budget");
    samples.copy_from_slice(pcm);
    AudioChunk::new(
        AudioChunkInfo {
            frames: u32::try_from(frame_count).expect("fixture frame count fits u32"),
            spec,
            ..Default::default()
        },
        samples,
    )
}

pub fn left_frames_from_chunks(chunks: impl IntoIterator<Item = AudioChunk>) -> Vec<f32> {
    chunks
        .into_iter()
        .flat_map(|chunk| {
            chunk
                .samples
                .chunks_exact(usize::from(GAPLESS_CHANNELS))
                .map(|frame| frame[0])
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Starts the first of one or two items on the deck and arms the second as
/// its gapless successor, as the queue does ahead of the first item's end.
/// Returns the identity the player will report back for each item, in the
/// order they were given.
pub async fn load_tagged_queue<const N: usize>(
    harness: &OfflinePlayer,
    items: [impl Into<TrackSource<TestPools>>; N],
) -> [TrackId; N] {
    let mut events = harness.player().subscribe();
    let mut items = items.into_iter();
    let first = items.next().map(Into::into);
    let second = items.next().map(Into::into);
    assert!(items.next().is_none(), "the deck holds at most two items");
    let ids: [TrackId; N] = harness
        .with_queue(move |queue| {
            let mut ids = Vec::new();
            for item in first.into_iter().chain(second) {
                ids.push(queue.append(item).expect("append gapless queue item"));
            }
            if let Some(id) = ids.first() {
                queue
                    .select(*id, Transition::None)
                    .expect("select first queue item");
                queue.play();
            }
            ids.try_into()
                .unwrap_or_else(|_| panic!("queue must retain every item identity"))
        })
        .await;
    for id in ids {
        crate::waits::wait_for_loader_done_event(
            &mut events,
            harness.player(),
            id,
            Duration::from_secs(1),
        )
        .await
        .expect("gapless queue source must finish loading");
    }
    ids
}

pub struct SyntheticSeamRender {
    pub left: Vec<f32>,
    pub events: Vec<TimedPlayerEvent>,
    pub first_frames: usize,
}

#[cfg(all(
    feature = "apple-fused-src",
    any(target_os = "macos", target_os = "ios")
))]
#[derive(Debug)]
pub struct AppleFusedSeamRender {
    pub control_db: f32,
    pub head_db: f32,
    pub nearby_db: [f32; 7],
    pub seam_db: f32,
    pub stitch_frame: usize,
}

#[cfg(all(
    feature = "apple-fused-src",
    any(target_os = "macos", target_os = "ios")
))]
#[derive(Debug)]
pub struct DrainedResource {
    pub output_frames: usize,
}

#[cfg(all(
    feature = "apple-fused-src",
    any(target_os = "macos", target_os = "ios")
))]
#[watchdog::hang_watchdog(timeout = Duration::from_secs(1))]
pub async fn drain_resource_to_eof(mut resource: Resource) -> DrainedResource {
    assert_eq!(
        resource.spec().sample_rate.get(),
        FUSED_FIXTURE_DEVICE_RATE,
        "Apple fused decoder must emit the host/device rate"
    );
    let mut output_frames = 0usize;
    let mut pending = 0usize;
    let deadline = WallInstant::now() + Duration::from_secs(20);
    loop {
        hang_tick!();
        match resource.next_chunk().expect("drain Apple fused resource") {
            ChunkOutcome::Chunk(chunk) => {
                hang_reset!();
                pending = 0;
                output_frames = output_frames.saturating_add(chunk.frames());
            }
            ChunkOutcome::Pending { .. } => {
                pending = pending.saturating_add(1);
                assert!(
                    pending < 4096,
                    "Apple fused resource stayed pending while draining to EOF"
                );
                assert!(
                    WallInstant::now() <= deadline,
                    "timed out draining Apple fused resource to EOF"
                );
                time::sleep(Duration::from_millis(1)).await;
            }
            ChunkOutcome::Eof { .. } => {
                return DrainedResource { output_frames };
            }
        }
    }
}

pub struct SyntheticPcmReader {
    pub bus: EventBus,
    pub duration_frames: usize,
    pub frames: Vec<f32>,
    pub metadata: TrackMetadata,
    pub position_frames: usize,
    pub spec: AudioSpec,
}

impl SyntheticPcmReader {
    fn new(frames: Vec<f32>, duration_frames: usize) -> Self {
        Self {
            bus: EventBus::default(),
            duration_frames,
            frames,
            metadata: TrackMetadata::default(),
            position_frames: 0,
            spec: AudioSpec::new(
                GAPLESS_CHANNELS,
                NonZeroU32::new(FUSED_FIXTURE_DEVICE_RATE).expect("test sample rate"),
            ),
        }
    }

    fn fill_planar(&mut self, output: &mut [&mut [f32]]) -> usize {
        let requested = output
            .iter()
            .map(|channel| channel.len())
            .min()
            .unwrap_or(0);
        let frames = requested.min(self.frames.len().saturating_sub(self.position_frames));
        for frame in 0..frames {
            let sample = self.frames[self.position_frames + frame];
            for channel in output.iter_mut() {
                channel[frame] = sample;
            }
        }
        self.position_frames += frames;
        frames
    }

    fn fill_interleaved(&mut self, output: &mut [f32]) -> usize {
        let channels = usize::from(self.spec.channels);
        let requested = output.len().saturating_div(channels.max(1));
        let frames = requested.min(self.frames.len().saturating_sub(self.position_frames));
        for frame in 0..frames {
            let sample = self.frames[self.position_frames + frame];
            let start = frame.saturating_mul(channels);
            let end = start.saturating_add(channels).min(output.len());
            output[start..end].fill(sample);
        }
        self.position_frames += frames;
        frames
    }

    fn read_outcome(&self, frames: usize) -> ReadOutcome {
        NonZeroUsize::new(frames).map_or_else(
            || ReadOutcome::Eof {
                position: self.position(),
            },
            |count| ReadOutcome::Frames {
                count,
                position: self.position(),
                source_span: None,
            },
        )
    }
}

impl AudioSession for SyntheticPcmReader {
    fn duration(&self) -> Option<Duration> {
        Some(duration_for_test_frames(self.duration_frames))
    }

    fn event_bus(&self) -> &EventBus {
        &self.bus
    }

    fn metadata(&self) -> &TrackMetadata {
        &self.metadata
    }
}

impl AudioRead for SyntheticPcmReader {
    fn next_chunk(&mut self) -> Result<ChunkOutcome, AudioReadError> {
        Ok(ChunkOutcome::Eof {
            position: self.position(),
        })
    }

    fn position(&self) -> Duration {
        duration_for_test_frames(self.position_frames)
    }

    fn read(&mut self, buf: &mut [f32]) -> Result<ReadOutcome, AudioReadError> {
        let frames = self.fill_interleaved(buf);
        Ok(self.read_outcome(frames * usize::from(self.spec.channels)))
    }

    fn read_planar<'a>(
        &mut self,
        output: &'a mut [&'a mut [f32]],
    ) -> Result<ReadOutcome, AudioReadError> {
        let frames = self.fill_planar(output);
        Ok(self.read_outcome(frames))
    }

    fn spec(&self) -> AudioSpec {
        self.spec
    }
}

impl AudioControl for SyntheticPcmReader {
    fn seek(&mut self, position: Duration) -> Result<SeekOutcome, AudioReadError> {
        let frames = frames_for_test_duration(position);
        if frames >= self.frames.len() {
            self.position_frames = self.frames.len();
            Ok(SeekOutcome::PastEof {
                target: position,
                duration: self.position(),
            })
        } else {
            self.position_frames = frames;
            Ok(SeekOutcome::Landed {
                target: position,
                landed_at: self.position(),
            })
        }
    }
}

pub async fn render_until_item_end(
    harness: &OfflinePlayer,
    terminal_item_id: TrackId,
) -> (Vec<f32>, Vec<TimedPlayerEvent>) {
    render_until_item_end_with_post_roll(harness, terminal_item_id, POST_ROLL_BLOCKS).await
}

#[watchdog::hang_watchdog(timeout = Duration::from_secs(1))]
pub async fn render_until_item_end_with_post_roll(
    harness: &OfflinePlayer,
    terminal_item_id: TrackId,
    post_roll_blocks: usize,
) -> (Vec<f32>, Vec<TimedPlayerEvent>) {
    let mut rendered = Vec::new();
    let mut rendered_frames = 0usize;
    let mut events = Vec::new();
    let mut track_start = None;

    loop {
        hang_tick!();
        let block = harness.render(BLOCK_FRAMES).await;
        if block.iter().any(|sample| sample.abs() > SILENCE_THRESHOLD) {
            hang_reset!();
        }
        rendered.extend_from_slice(&block);
        rendered_frames = rendered_frames.saturating_add(BLOCK_FRAMES);
        if track_start.is_none() {
            track_start = harness
                .deck_snapshot()
                .slots
                .iter()
                .filter_map(|slot| slot.mark)
                .filter(|mark| mark.lane.frame > 0)
                .find_map(|mark| {
                    mark.session
                        .frames_since(kithara::signal::SessionFrame::new(0))?
                        .checked_sub(mark.lane.frame)
                });
        }
        events.extend(
            harness
                .tick_and_drain()
                .await
                .into_iter()
                .map(|event| TimedPlayerEvent::new(rendered_frames, event)),
        );

        if events.iter().any(|event| {
            matches!(
                &event.event,
                PlayerEvent::ItemDidPlayToEnd { item } if item.id() == terminal_item_id
            )
        }) {
            for _ in 0..post_roll_blocks {
                hang_tick!();
                let block = harness.render(BLOCK_FRAMES).await;
                rendered.extend_from_slice(&block);
                rendered_frames = rendered_frames.saturating_add(BLOCK_FRAMES);
                events.extend(
                    harness
                        .tick_and_drain()
                        .await
                        .into_iter()
                        .map(|event| TimedPlayerEvent::new(rendered_frames, event)),
                );
            }
            let start = usize::try_from(track_start.expect("the ended track must have started"))
                .expect("track start fits usize");
            rendered.drain(..start * usize::from(GAPLESS_CHANNELS));
            for event in &mut events {
                event.frame_end = event.frame_end.saturating_sub(start);
            }
            return (rendered, events);
        }

        assert!(
            rendered_frames < ITEM_END_BUDGET_FRAMES,
            "{terminal_item_id} never finished within {ITEM_END_BUDGET_FRAMES} rendered \
             frames; events={events:?}"
        );
        // A yield, not a wait: the loop renders what it waits on, and under
        // flash this is where the engine gets to run everything else.
        time::sleep(Duration::from_millis(5)).await;
    }
}

pub fn timed_event_frame_end<P>(events: &[TimedPlayerEvent], predicate: P) -> Option<usize>
where
    P: Fn(&PlayerEvent) -> bool,
{
    events
        .iter()
        .find(|timed| predicate(&timed.event))
        .map(|timed| timed.frame_end)
}

pub fn peak_first_diff(left: &[f32], center: usize, half: usize) -> f32 {
    assert!(
        (1..left.len()).contains(&center),
        "first-difference center must be in 1..{}, got {center}",
        left.len(),
    );
    let start = center.saturating_sub(half).max(1);
    let end = center.saturating_add(half).min(left.len() - 1);
    let mut peak = 0.0_f32;
    for i in start..=end {
        peak = peak.max((left[i] - left[i - 1]).abs());
    }
    peak
}

pub fn gapless_control_peak(left: &[f32], stitch_frame: usize) -> (f32, usize) {
    let mut peak = 0.0_f32;
    let mut count = 0usize;
    for center in [
        stitch_frame.saturating_sub(ContinuityMetric::CONTROL_STRIDE_FRAMES),
        stitch_frame.saturating_add(ContinuityMetric::CONTROL_STRIDE_FRAMES),
    ] {
        if center <= ContinuityMetric::HALF_WINDOW_FRAMES
            || center + ContinuityMetric::HALF_WINDOW_FRAMES >= left.len()
        {
            continue;
        }
        peak = peak.max(peak_first_diff(
            left,
            center,
            ContinuityMetric::HALF_WINDOW_FRAMES,
        ));
        count += 1;
    }
    (peak, count)
}

#[cfg(all(
    feature = "apple-fused-src",
    any(target_os = "macos", target_os = "ios")
))]
pub fn floor_scaled_frames(frames: u64, output_rate: u32, input_rate: u32) -> u64 {
    let numerator = u128::from(frames).saturating_mul(u128::from(output_rate));
    let scaled = numerator / u128::from(input_rate.max(1));
    u64::try_from(scaled).unwrap_or(u64::MAX)
}

pub fn ceil_scaled_frames(frames: u64, output_rate: u32, input_rate: u32) -> u64 {
    let numerator = u128::from(frames).saturating_mul(u128::from(output_rate));
    let denominator = u128::from(input_rate.max(1));
    let scaled = numerator.saturating_add(denominator - 1) / denominator;
    u64::try_from(scaled).unwrap_or(u64::MAX)
}

pub fn seam_step_db(left: &[f32], stitch_frame: usize) -> f32 {
    assert!(
        (1..left.len()).contains(&stitch_frame),
        "stitch frame must be in 1..{}, got {stitch_frame}",
        left.len(),
    );
    amplitude_db((left[stitch_frame] - left[stitch_frame - 1]).abs())
}

#[cfg(all(
    feature = "apple-fused-src",
    any(target_os = "macos", target_os = "ios")
))]
pub fn nearby_seam_step_db(left: &[f32], stitch_frame: usize) -> [f32; 7] {
    let mut values = [0.0_f32; 7];
    let start = stitch_frame.saturating_sub(3);
    for (index, value) in values.iter_mut().enumerate() {
        *value = seam_step_db(left, start.saturating_add(index));
    }
    values
}

pub fn amplitude_db(value: f32) -> f32 {
    20.0 * value.max(1.0e-9).log10()
}

pub fn duration_for_test_frames(frames: usize) -> Duration {
    let nanos = u128::try_from(frames)
        .unwrap_or(u128::MAX)
        .saturating_mul(1_000_000_000)
        / u128::from(FUSED_FIXTURE_DEVICE_RATE);
    Duration::from_nanos(u64::try_from(nanos).unwrap_or(u64::MAX))
}

pub fn frames_for_test_duration(duration: Duration) -> usize {
    let frames = duration
        .as_nanos()
        .saturating_mul(u128::from(FUSED_FIXTURE_DEVICE_RATE))
        / 1_000_000_000;
    usize::try_from(frames).unwrap_or(usize::MAX)
}

/// Index just past the last sample whose absolute amplitude is at or above
/// `threshold`. Used to anchor "rendered length" on the actual audible PCM
/// region rather than on the `ItemDidPlayToEnd` event observation frame —
/// the latter trails the audio thread by ~100 ms in the offline harness.
pub fn audio_end_frame(samples: &[f32], threshold: f32) -> usize {
    samples
        .iter()
        .rposition(|sample| sample.abs() >= threshold)
        .map_or(0, |idx| idx + 1)
}

pub fn rms(samples: &[f32], start: usize, end: usize) -> f32 {
    let end = end.min(samples.len());
    if end <= start {
        return 0.0;
    }
    let slice = &samples[start..end];
    let sum_sq: f64 = slice.iter().map(|s| f64::from(*s) * f64::from(*s)).sum();
    let length = num_traits::ToPrimitive::to_f64(&slice.len()).expect("RMS length fits f64");
    num_traits::ToPrimitive::to_f32(&(sum_sq / length).sqrt()).expect("RMS fits f32")
}

pub fn assert_close_to(
    actual: usize,
    expected: usize,
    tolerance: usize,
    label: &str,
    events: &[TimedPlayerEvent],
) {
    let delta = actual.abs_diff(expected);
    assert!(
        delta <= tolerance,
        "{label}: actual={actual}, expected={expected}, delta={delta}, tolerance={tolerance}; \
         events={events:?}"
    );
}
