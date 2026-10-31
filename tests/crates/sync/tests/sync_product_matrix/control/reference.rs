use kithara::{
    audio::{AudioObserveError, AudioObserver},
    bufpool::SampleBuffer,
    download::Downloader,
    effects::LimiterConfig,
    play::{DeckMixSettings, Position, Settled},
    signal::{AudioChunk, AudioChunkInfo},
};
use kithara_integration_tests::{
    offline::{OfflinePlayer, OfflinePlayerOptions, linked::LinkReceiptOrigin},
    pcm_oracle::Audio,
};
use kithara_render::bridge::DeckSnapshot;
use kithara_test_utils::bufpool::{Pools, pools_with_budget, sample_buffer};
use num_traits::ToPrimitive;
use ringbuf::{
    HeapCons, HeapProd, HeapRb,
    traits::{Consumer, Observer, Producer, Split},
};

use super::{
    super::{cases::*, consumed, imports::*},
    sampling::{self, SINC_RADIUS},
};

pub(in super::super) const CONTROL_GAIN: f32 = 0.25;
const CONTROL_CAPTURE_FRAMES: usize = 128;

pub(in super::super) struct ControlCapture {
    pub(in super::super) pcm: Vec<f32>,
    item: u64,
    rate: f32,
    points: Vec<ControlPoint>,
    requested: Position,
    landing: Result<(SessionFrame, u64), String>,
    source: Result<(), String>,
    decoded: Arc<Mutex<DecodedRecorder>>,
}

#[derive(Clone)]
struct ControlPoint {
    frame: usize,
    rendered_frames: usize,
    session: u64,
    deck: DeckSnapshot,
}

impl ControlCapture {
    pub(in super::super) fn evidence(&self) -> serde_json::Value {
        serde_json::json!({
            "rate": self.rate,
            "requested_mix_gain": CONTROL_GAIN,
            "requested_ns": self.requested.as_nanos().to_string(),
            "landing": format!("{:?}", self.landing),
            "requested_source": format!("{:?}", self.source),
            "decoded_chunks": self.decoded.lock().view().chunks.len(),
            "retained_frames": self.pcm.len() / usize::from(CHANNELS),
            "points": self.points.iter().map(|point| {
                serde_json::json!({
                    "frame": point.frame,
                    "rendered_frames": point.rendered_frames,
                    "session": point.session,
                    "output_rate": point.deck.sample_rate,
                    "metrics": format!("{:?}", point.deck.metrics),
                    "slots": point.deck.slots.iter().map(|slot| {
                        let consumed = slot.mark.zip(slot.source).and_then(|(mark, info)| {
                            let offset = mark.lane.frame.checked_sub(info.lane_frame)?;
                            let span = info.source_span?;
                            if mark.lane.segment != info.segment {
                                return None;
                            }
                            let (numerator, denominator) = span.source_ratio_at(offset)?;
                            Some(serde_json::json!({
                                "packet_offset": offset,
                                "source_numerator": numerator.to_string(),
                                "source_denominator": denominator.get().to_string(),
                                "source_rate": span.sample_rate().get(),
                            }))
                        });
                        serde_json::json!({
                            "state": format!("{:?}", slot.state),
                            "gain": slot.gain,
                            "mark": slot.mark.map(|mark| serde_json::json!({
                                "session": i64::from(mark.session),
                                "segment": mark.lane.segment.get(),
                                "lane_frame": mark.lane.frame,
                                "raw_ns": mark.position.as_nanos().to_string(),
                            })),
                            "source": slot.source.map(|info| serde_json::json!({
                                "spec": format!("{:?}", info.spec),
                                "sample_rate": info.spec.sample_rate.get(),
                                "channels": info.spec.channels,
                                "segment": info.segment.get(),
                                "source_revision": info.source_revision,
                                "render_revision": info.render_revision,
                                "mapping_revision": info.mapping_revision.map(std::num::NonZeroU64::get),
                                "lane_frame": info.lane_frame,
                                "frames": info.frames,
                                "frame_offset": info.frame_offset,
                                "media_origin_ns": info.media_origin.map(|origin| origin.as_nanos().to_string()),
                                "span": format!("{:?}", info.source_span),
                            })),
                            "consumed": consumed,
                        })
                    }).collect::<Vec<_>>(),
                })
            }).collect::<Vec<_>>(),
        })
    }

    pub(in super::super) fn source_content_failures(
        &self,
        reference: &Audio,
        wrong_source: &Audio,
        grid: &BeatGridModel,
    ) -> Vec<String> {
        self.check_source_content(reference, wrong_source, grid)
            .err()
            .into_iter()
            .collect()
    }

    pub(in super::super) fn observed_content_failures(
        &self,
        wrong_source: &Audio,
        grid: &BeatGridModel,
    ) -> Vec<String> {
        self.check_observed_content(wrong_source, grid)
            .err()
            .into_iter()
            .collect()
    }

    fn check_observed_content(
        &self,
        wrong_source: &Audio,
        grid: &BeatGridModel,
    ) -> Result<(), String> {
        let first = self
            .points
            .first()
            .ok_or_else(|| "control has no original capture clock".to_owned())?;
        let slot = playing_slot(first)?;
        let info = slot
            .source
            .ok_or_else(|| "control has no observed source spec".to_owned())?;
        let origin = info
            .media_origin
            .ok_or_else(|| "control has no raw media-origin custody".to_owned())?;
        self.source_law_on(info.spec, origin, &self.points)?;
        self.check_clock_guards_with(|points| {
            self.source_law_on(info.spec, origin, points).map(|_| ())
        })?;
        let marks = self
            .points
            .iter()
            .map(|point| {
                let session = i64::try_from(point.session).map_err(|error| error.to_string())?;
                Ok((SessionFrame::new(session), playing_slot(point)?))
            })
            .collect::<Result<Vec<_>, String>>()?;
        let consumed = consumed::Capture::for_track(self.item, first.deck.sample_rate)?;
        let mut recording = self.decoded.lock();
        let decoded = recording.view();
        decoded.check_physical(&self.pcm, &marks, &consumed, grid, wrong_source, None)?;
        drop(recording);
        let retained = i64::from(marks.first().expect("capture has an initial mark").0)
            ..i64::from(marks.last().expect("capture has a final mark").0);
        consumed::check_trace_negatives(self.item, first.deck.sample_rate, retained)
    }

    fn source_law(
        &self,
        reference: &Audio,
        points: &[ControlPoint],
    ) -> Result<(u128, (u128, u128)), String> {
        if reference.sample_rate != 48_000 || reference.channels != CHANNELS {
            return Err(format!(
                "control native spec is {}/{} instead of 48000/{CHANNELS}",
                reference.sample_rate, reference.channels
            ));
        }
        if reference.samples.is_empty()
            || !reference
                .samples
                .len()
                .is_multiple_of(usize::from(CHANNELS))
        {
            return Err("control native reference has no complete PCM frames".to_owned());
        }
        self.source_law_on(
            AudioSpec::new(
                CHANNELS,
                NonZeroU32::new(reference.sample_rate).expect("native reference rate"),
            ),
            Duration::ZERO,
            points,
        )
    }

    fn source_law_on(
        &self,
        spec: AudioSpec,
        media_origin: Duration,
        points: &[ControlPoint],
    ) -> Result<(u128, (u128, u128)), String> {
        self.source.as_ref().map_err(Clone::clone)?;
        let &(landed, segment) = self.landing.as_ref().map_err(Clone::clone)?;
        let first = points
            .first()
            .ok_or_else(|| "control has no consumed points".to_owned())?;
        if first.frame != 0 || points.len() < 2 {
            return Err("control has no complete capture clock ledger".to_owned());
        }
        let first_slot = playing_slot(first)?;
        let first_mark = first_slot
            .mark
            .ok_or_else(|| "control has no initial consumed mark".to_owned())?;
        let first_source = first_slot
            .source
            .ok_or_else(|| "control has no initial source revision".to_owned())?;
        let landed = u64::try_from(i64::from(landed)).map_err(|error| error.to_string())?;
        let consumption_start = first
            .session
            .checked_sub(first_mark.lane.frame)
            .ok_or_else(|| "control lane origin is outside the session clock".to_owned())?;
        if consumption_start < landed {
            return Err("control PCM consumption precedes its actual Adopt receipt".to_owned());
        }
        let scalar = scalar_ratio(self.rate)?;
        let rate = (
            scalar
                .0
                .checked_mul(1_000_000_000)
                .ok_or("control scalar numerator overflows")?,
            scalar
                .1
                .checked_mul(1_000_000_000)
                .ok_or("control scalar denominator overflows")?,
        );
        let source = self
            .requested
            .checked_add(media_origin)
            .ok_or_else(|| "control requested raw cue overflows".to_owned())?
            .as_nanos()
            .checked_mul(u128::from(spec.sample_rate.get()))
            .and_then(|value| value.checked_mul(scalar.1))
            .ok_or_else(|| "control requested source numerator overflows".to_owned())?;
        let origin = source
            .checked_add(
                u128::from(first_mark.lane.frame)
                    .checked_mul(rate.0)
                    .ok_or("control lane source advance overflows")?,
            )
            .ok_or_else(|| "control source origin overflows".to_owned())?;
        self.check_capture_clock(points)?;
        for point in points {
            if point.deck.sample_rate != spec.sample_rate.get()
                || point.session != first.session + point.frame as u64
            {
                return Err(format!(
                    "control frame {} lost the independent output clock",
                    point.frame
                ));
            }
            let slot = playing_slot(point)?;
            if slot.gain != 1.0 {
                return Err(format!(
                    "control frame {} has gain {}",
                    point.frame, slot.gain
                ));
            }
            let mark = slot
                .mark
                .ok_or_else(|| "control has no consumed mark".to_owned())?;
            let info = slot
                .source
                .ok_or_else(|| "control has no consumed source".to_owned())?;
            if mark.lane.segment.get() != segment
                || info.segment.get() != segment
                || i64::from(mark.session) != point.session as i64
                || info.media_origin != Some(media_origin)
                || info.spec != spec
            {
                return Err(format!(
                    "control frame {} lost requested segment/native axes",
                    point.frame
                ));
            }
            if point.session.checked_sub(mark.lane.frame) != Some(consumption_start)
                || first_mark.lane.frame.checked_add(point.frame as u64) != Some(mark.lane.frame)
            {
                return Err(format!(
                    "control frame {} changes its consumed lane/session clock",
                    point.frame
                ));
            }
            if (
                info.source_revision,
                info.render_revision,
                info.mapping_revision,
            ) != (
                first_source.source_revision,
                first_source.render_revision,
                first_source.mapping_revision,
            ) {
                return Err(format!(
                    "control frame {} changes its source/render/mapping revision",
                    point.frame
                ));
            }
            let offset = mark
                .lane
                .frame
                .checked_sub(info.lane_frame)
                .ok_or_else(|| "control mark precedes its source span".to_owned())?;
            let span = info
                .source_span
                .ok_or_else(|| "control source has no exact span".to_owned())?;
            if span.sample_rate() != spec.sample_rate {
                return Err("control source span has a different native clock".to_owned());
            }
            if u64::from(info.frames) != span.output_frames() {
                return Err(
                    "control source packet and exact span cover different intervals".to_owned(),
                );
            }
            let (numerator, denominator) = span
                .source_ratio_at(offset)
                .ok_or_else(|| "control mark is outside its exact source span".to_owned())?;
            let expected = origin + point.frame as u128 * rate.0;
            let actual_cross = numerator
                .checked_mul(rate.1)
                .ok_or_else(|| "control consumed rational overflows".to_owned())?;
            let expected_cross = expected
                .checked_mul(denominator.get())
                .ok_or_else(|| "control expected rational overflows".to_owned())?;
            if actual_cross != expected_cross {
                return Err(format!(
                    "control frame {} consumes {numerator}/{} instead of requested {expected}/{}",
                    point.frame,
                    denominator.get(),
                    rate.1
                ));
            }
            self.check_packet_law(info, span, source, rate)?;
        }
        Ok((origin, rate))
    }

    fn check_capture_clock(&self, points: &[ControlPoint]) -> Result<(), String> {
        for pair in points.windows(2) {
            if !(1..=CONTROL_CAPTURE_FRAMES).contains(&pair[1].rendered_frames)
                || pair[0].frame.checked_add(pair[1].rendered_frames) != Some(pair[1].frame)
            {
                return Err(
                    "control capture clock ledger omits or reorders a render block".to_owned(),
                );
            }
        }
        if points
            .last()
            .is_none_or(|point| point.frame < self.pcm.len() / usize::from(CHANNELS))
        {
            return Err("control consumed clock ledger does not cover retained PCM".to_owned());
        }
        Ok(())
    }

    fn check_packet_law(
        &self,
        info: AudioChunkInfo,
        span: kithara::signal::SourceSpan,
        source: u128,
        rate: (u128, u128),
    ) -> Result<(), String> {
        for output in 0..=span.output_frames() {
            let (actual, denominator) = span
                .source_ratio_at(output)
                .ok_or_else(|| "control packet has a missing constant-law coordinate".to_owned())?;
            let lane_frame = u128::from(info.lane_frame)
                .checked_add(u128::from(output))
                .ok_or_else(|| "control packet lane coordinate overflows".to_owned())?;
            let expected = source
                .checked_add(
                    lane_frame
                        .checked_mul(rate.0)
                        .ok_or("control packet source advance overflows")?,
                )
                .ok_or_else(|| "control requested constant-law coordinate overflows".to_owned())?;
            let actual_cross = actual
                .checked_mul(rate.1)
                .ok_or_else(|| "control packet source ratio overflows".to_owned())?;
            let expected_cross = expected
                .checked_mul(denominator.get())
                .ok_or_else(|| "control requested packet ratio overflows".to_owned())?;
            if actual_cross != expected_cross {
                return Err(format!(
                    "control packet at lane frame {lane_frame} violates the requested constant scalar {}",
                    self.rate
                ));
            }
        }
        Ok(())
    }

    fn check_clock_guards(&self, reference: &Audio) -> Result<(), String> {
        self.check_clock_guards_with(|points| self.source_law(reference, points).map(|_| ()))
    }

    fn check_clock_guards_with(
        &self,
        law: impl Fn(&[ControlPoint]) -> Result<(), String>,
    ) -> Result<(), String> {
        for wrong_axis in ["lane frame", "Host frame", "capture gap", "source revision"] {
            let mut changed = self.points.clone();
            let point = changed
                .get_mut(1)
                .ok_or_else(|| "control has no interior clock guard".to_owned())?;
            match wrong_axis {
                "Host frame" => point.session += 1,
                "capture gap" => point.frame += 1,
                _ => {
                    let slot = point
                        .deck
                        .slots
                        .iter_mut()
                        .find(|slot| slot.state == kithara_render::bridge::SlotState::Playing)
                        .ok_or_else(|| "control clock guard has no Playing slot".to_owned())?;
                    if wrong_axis == "lane frame" {
                        slot.mark
                            .as_mut()
                            .ok_or_else(|| "control clock guard has no mark".to_owned())?
                            .lane
                            .frame += 1;
                    } else {
                        slot.source
                            .as_mut()
                            .ok_or_else(|| "control clock guard has no source".to_owned())?
                            .source_revision += 1;
                    }
                }
            }
            if law(&changed).is_ok() {
                return Err(format!(
                    "control accepts a wrong {wrong_axis} with unchanged PCM"
                ));
            }
        }
        Ok(())
    }

    fn check_source_content(
        &self,
        reference: &Audio,
        wrong_source: &Audio,
        grid: &BeatGridModel,
    ) -> Result<(), String> {
        let (origin, rate) = self.source_law(reference, &self.points)?;
        self.check_clock_guards(reference)?;
        let frames = self.pcm.len() / usize::from(CHANNELS);
        if frames == 0 || !self.pcm.len().is_multiple_of(usize::from(CHANNELS)) {
            return Err("control contains no complete retained frames".to_owned());
        }
        let mut expected = Vec::with_capacity(self.pcm.len());
        for frame in 0..frames {
            let position = (origin + frame as u128 * rate.0, rate.1);
            for sample in source_samples(reference, position, self.rate, [0, 1])? {
                expected.push(output_sample(sample * CONTROL_GAIN)?);
            }
        }
        compare_pcm(&self.pcm, &expected)?;
        if !expected.iter().any(|sample| *sample != 0.0) {
            return Err("control content reference is entirely silent".to_owned());
        }
        check_content_guards(
            reference,
            wrong_source,
            grid,
            &expected,
            origin,
            rate,
            self.rate,
        )?;
        Ok(())
    }
}

fn playing_slot(point: &ControlPoint) -> Result<kithara_render::bridge::SlotSnapshot, String> {
    let mut playing = point
        .deck
        .slots
        .iter()
        .filter(|slot| slot.state == kithara_render::bridge::SlotState::Playing);
    let slot = playing
        .next()
        .copied()
        .ok_or_else(|| format!("control frame {} has no Playing slot", point.frame))?;
    if playing.next().is_some() {
        return Err(format!(
            "control frame {} has multiple Playing slots",
            point.frame
        ));
    }
    Ok(slot)
}

fn scalar_ratio(rate: f32) -> Result<(u128, u128), String> {
    if !rate.is_finite() || rate <= 0.0 || rate.is_subnormal() {
        return Err(format!("invalid control scalar {rate}"));
    }
    let bits = rate.to_bits();
    let mantissa = u128::from((bits & 0x7f_ffff) | 0x80_0000);
    let exponent = ((bits >> 23) & 0xff) as i32 - 150;
    if exponent >= 0 {
        mantissa
            .checked_shl(exponent.unsigned_abs())
            .map(|value| (value, 1))
    } else {
        1_u128
            .checked_shl(exponent.unsigned_abs())
            .map(|value| (mantissa, value))
    }
    .ok_or_else(|| "control scalar rational overflows".to_owned())
}

fn source_sample(
    reference: &Audio,
    position: (u128, u128),
    speed: f32,
    channel: usize,
) -> Result<f32, String> {
    source_samples(reference, position, speed, [channel]).map(|[sample]| sample)
}

fn source_samples<const N: usize>(
    reference: &Audio,
    position: (u128, u128),
    speed: f32,
    channels: [usize; N],
) -> Result<[f32; N], String> {
    let source = i64::try_from(position.0 / position.1).map_err(|error| error.to_string())?;
    mapped_samples(position, f64::from(speed), |offset| {
        let frame = source
            .checked_add(offset)
            .and_then(|frame| usize::try_from(frame).ok());
        let mut samples = [0.0; N];
        for (sample, channel) in samples.iter_mut().zip(channels) {
            *sample = frame
                .and_then(|frame| frame.checked_mul(usize::from(reference.channels)))
                .and_then(|index| index.checked_add(channel))
                .and_then(|index| reference.samples.get(index))
                .copied()
                .filter(|sample| sample.is_finite())
                .ok_or_else(|| {
                    format!(
                        "native source does not cover frame {source}+{offset}, channel {channel}"
                    )
                })?;
        }
        Ok(samples)
    })
}

fn mapped_sample(
    position: (u128, u128),
    speed: f64,
    mut sample: impl FnMut(i64) -> Result<f32, String>,
) -> Result<f32, String> {
    mapped_samples(position, speed, |offset| {
        sample(offset).map(|sample| [sample])
    })
    .map(|[sample]| sample)
}

pub(super) fn mapped_samples<const N: usize>(
    position: (u128, u128),
    speed: f64,
    sample: impl FnMut(i64) -> Result<[f32; N], String>,
) -> Result<[f32; N], String> {
    sampling::SampleKernel::new(position, speed)?.samples(sample)
}

fn output_sample(sample: f32) -> Result<f32, String> {
    if !sample.is_finite() || sample.abs() >= LimiterConfig::default().ceiling() {
        return Err("control reference exceeds its configured output headroom".to_owned());
    }
    Ok(kithara::signal::sanitize_sample(sample))
}

pub(in super::super) fn compare_pcm(actual: &[f32], expected: &[f32]) -> Result<(), String> {
    if actual.len() != expected.len() || actual.is_empty() {
        return Err(format!(
            "control PCM domain differs: actual {}, expected {}",
            actual.len(),
            expected.len()
        ));
    }
    if let Some((index, (&actual, &expected))) =
        actual
            .iter()
            .zip(expected)
            .enumerate()
            .find(|(_, (actual, expected))| {
                !actual.is_finite() || !expected.is_finite() || actual != expected
            })
    {
        return Err(format!(
            "control source content differs at sample {index}: {actual} instead of {expected}"
        ));
    }
    Ok(())
}

fn check_content_guards(
    reference: &Audio,
    wrong_source: &Audio,
    grid: &BeatGridModel,
    expected: &[f32],
    origin: u128,
    rate: (u128, u128),
    speed: f32,
) -> Result<(), String> {
    let channels = usize::from(CHANNELS);
    let frames = expected.len() / channels;
    if frames < 3 {
        return Err("control has no equal-domain one-frame guard".to_owned());
    }
    let interior = &expected[channels..expected.len() - channels];
    for (name, shifted) in [
        (
            "one frame early",
            &expected[..expected.len() - 2 * channels],
        ),
        ("one frame late", &expected[2 * channels..]),
    ] {
        if compare_pcm(shifted, interior).is_ok() {
            return Err(format!("control source cannot reject PCM {name}"));
        }
    }
    for wrong_speed in [speed * 0.5, speed * 2.0, speed + 0.01] {
        let wrong_rate = scalar_ratio(wrong_speed)?;
        let window = frames.min(BLOCK_FRAMES);
        let mut wrong = Vec::with_capacity(window * channels);
        for frame in 0..window {
            let position = (
                origin * wrong_rate.1 + frame as u128 * wrong_rate.0 * rate.1,
                rate.1 * wrong_rate.1,
            );
            for sample in source_samples(reference, position, wrong_speed, [0, 1])? {
                wrong.push(output_sample(sample * CONTROL_GAIN)?);
            }
        }
        if compare_pcm(&wrong, &expected[..window * channels]).is_ok() {
            return Err(format!(
                "control source cannot reject wrong PCM scalar {wrong_speed}"
            ));
        }
    }
    let window = frames.min(BLOCK_FRAMES);
    for (name, source, source_origin) in [
        ("wrong cue", reference, origin + rate.1),
        (
            "wrong source",
            wrong_source,
            u128::from(SINC_RADIUS.unsigned_abs()) * rate.1,
        ),
    ] {
        if source.sample_rate != reference.sample_rate || source.channels != reference.channels {
            return Err(format!(
                "{name} negative fixture has a different native spec"
            ));
        }
        let mut wrong = Vec::with_capacity(window * channels);
        for frame in 0..window {
            let position = (source_origin + frame as u128 * rate.0, rate.1);
            for sample in source_samples(source, position, speed, [0, 1])? {
                wrong.push(output_sample(sample * CONTROL_GAIN)?);
            }
        }
        if compare_pcm(&wrong, &expected[..window * channels]).is_ok() {
            return Err(format!(
                "control source cannot reject {name} PCM with unchanged metadata"
            ));
        }
    }
    for wrong_gain in [0.5, 1.0] {
        let wrong: Vec<_> = expected
            .iter()
            .map(|sample| sample * (wrong_gain / CONTROL_GAIN))
            .collect();
        if compare_pcm(&wrong, expected).is_ok() {
            return Err(format!(
                "control source cannot reject wrong PCM gain {wrong_gain}"
            ));
        }
    }
    let mut missing = expected.to_vec();
    missing[..window * channels].fill(0.0);
    if compare_pcm(&missing, expected).is_ok() {
        return Err("control source cannot reject a removed content window".to_owned());
    }
    for beat in &grid.as_raw().beats {
        let native = (beat.at * f64::from(reference.sample_rate))
            .round()
            .to_u128()
            .ok_or_else(|| "control supplied beat has no native frame".to_owned())?;
        let Some(offset) = native
            .checked_mul(rate.1)
            .and_then(|frame| frame.checked_sub(origin))
        else {
            continue;
        };
        let frame = usize::try_from(offset / rate.0).map_err(|error| error.to_string())?;
        if frame < 128 || frame + 128 > frames {
            continue;
        }
        let body = &expected[(frame - 128) * channels..(frame + 128) * channels];
        let removed = vec![0.0; body.len()];
        if compare_pcm(&removed, body).is_ok() {
            return Err(format!(
                "control source cannot reject removed supplied beat {}",
                beat.ordinal
            ));
        }
    }
    Ok(())
}

#[kithara_test_utils::kithara::hang_watchdog(timeout = Duration::from_secs(5), ctx = String)]
pub(in super::super) async fn capture(
    source: String,
    downloader: Downloader,
    sample_rate: u32,
    native_bpm: f64,
    target_bpm: f64,
    start: f64,
) -> (kithara_test_utils::test::usdt::Scope, ControlCapture) {
    let trace = kithara_test_utils::test::usdt::scope();
    let observation = Arc::new(Mutex::new(LinkObservation::default()));
    let session = HostConfig::offline(pools())
        .settings(
            HostSettings::builder()
                .sample_rate(NonZeroU32::new(sample_rate).expect("control output rate"))
                .build(),
        )
        .build();
    let harness = OfflinePlayer::with_factory(
        OfflinePlayerOptions::builder()
            .eq_layout(Vec::new())
            .mix(DeckMixSettings::builder().level(CONTROL_GAIN).build())
            .crossfade_duration(0.0)
            .block_on_underrun(true)
            .warp(
                kithara::warp::WarpConfig::builder()
                    .backend(kithara::warp::StretchKind::Glide)
                    .build(),
            )
            .build(),
        session,
        ObservedFactory::new(observation.clone()),
    )
    .await;
    let _ = harness.render(BLOCK_FRAMES).await;
    let rate: f32 = (target_bpm / native_bpm).as_();
    harness
        .with_queue(move |control| control.set_default_rate(rate))
        .await
        .expect("control rate");
    let config = ResourceConfig::for_src(ResourceSrc::parse(&source).expect("control source"))
        .downloader(downloader)
        .store(memory_asset_store())
        .initial_abr_mode(AbrMode::manual(0))
        .build();
    let id = harness.load_config(config).await;
    let (decoded, observer) = DecodedRecorder::open();
    let requested_source = source.clone();
    let source_custody = harness
        .with_queue(move |control| {
            let matches = control.track_source(id).as_ref().and_then(TrackSource::uri)
                == Some(requested_source.as_str());
            control.attach_observer(id, observer);
            if matches {
                Ok(())
            } else {
                Err("control selected source differs from the originally requested URI".to_owned())
            }
        })
        .await;
    harness
        .render_until_current(
            id,
            BLOCK_FRAMES,
            time::WallInstant::now() + Duration::from_secs(5),
        )
        .await;
    let requested = Position::try_from_secs_f64(start).expect("original control cue");
    let seek_index = observation.lock().seeks.len();
    let command_index = observation.lock().timeline.len();
    harness.seek(start).await;
    let capture_frames: usize = (f64::from(sample_rate) * 60.0 / target_bpm * 6.0)
        .round()
        .as_();
    for block in 0..(capture_frames / 12).div_ceil(BLOCK_FRAMES) {
        hang_tick!(format!(
            "provider control {target_bpm}: settling block {block}"
        ));
        let _ = harness.render(BLOCK_FRAMES).await;
        let _ = harness.tick_and_drain().await;
        hang_reset!();
    }
    let mut captured = Vec::with_capacity(capture_frames * usize::from(CHANNELS));
    let observe = |frame, rendered_frames| ControlPoint {
        frame,
        rendered_frames,
        session: harness.host().position(),
        deck: harness.deck_snapshot(),
    };
    let mut points = vec![observe(0, 0)];
    while captured.len() < capture_frames * usize::from(CHANNELS) {
        hang_tick!(format!(
            "provider control {source} at {target_bpm}: {} samples",
            captured.len()
        ));
        let remaining = capture_frames - captured.len() / usize::from(CHANNELS);
        let point = points.last().expect("control has an initial capture point");
        let slot = playing_slot(point).expect("control has one Playing source");
        let mark = slot.mark.expect("control has a consumed lane mark");
        let info = slot.source.expect("control has consumed source metadata");
        let span = info.source_span.expect("control has an exact source span");
        let packet_remaining = info
            .lane_frame
            .checked_add(span.output_frames())
            .and_then(|end| end.checked_sub(mark.lane.frame))
            .expect("control mark lies within its consumed source span");
        let frames = remaining.min(CONTROL_CAPTURE_FRAMES).min(
            usize::try_from(packet_remaining.max(1)).expect("control packet length fits usize"),
        );
        let block = harness.render(frames).await;
        assert_eq!(block.len(), frames * usize::from(CHANNELS));
        captured.extend(block);
        let _ = harness.tick_and_drain().await;
        points.push(observe(captured.len() / usize::from(CHANNELS), frames));
        hang_reset!();
    }
    captured.truncate(capture_frames * usize::from(CHANNELS));
    harness.close().await;
    let landing = (|| {
        let observation = observation.lock();
        let requests: Vec<_> = observation.seeks[seek_index..]
            .iter()
            .filter(|(item, origin, _)| *item == id && *origin == LinkReceiptOrigin::Deck)
            .collect();
        let [(_, _, seq)] = requests.as_slice() else {
            return Err(format!(
                "original control seek has {} sequences",
                requests.len()
            ));
        };
        let cue = requested.as_nanos().to_string();
        let requests: Vec<_> = observation.timeline[command_index..]
            .iter()
            .filter(|entry| {
                entry.get("item").and_then(serde_json::Value::as_u64) == Some(u64::from(id))
            })
            .filter_map(|entry| entry.get("details").and_then(|details| details.get("Seek")))
            .collect();
        if requests.len() != 1 || requests[0].as_str() != Some(cue.as_str()) {
            return Err("original control seek changed the requested cue".to_owned());
        }
        let receipts: Vec<_> = observation.placement_receipts.iter().filter_map(|(item, origin, receipt)| {
            (*item == id && *origin == LinkReceiptOrigin::Deck).then_some(receipt).filter(|receipt| matches!(receipt, Settled::Applied { seq: answered, .. } | Settled::Rejected { seq: answered, .. } if answered == seq))
        }).collect();
        match receipts.as_slice() {
            [Settled::Applied { at, .. }] => {
                let at = *at;
                let segments: Vec<_> = observation.timeline[command_index..]
                    .iter()
                    .filter(|entry| {
                        entry.get("event").and_then(serde_json::Value::as_str)
                            == Some("deck receipt")
                            && entry.get("item").and_then(serde_json::Value::as_u64)
                                == Some(u64::from(id))
                            && entry
                                .get("details")
                                .and_then(|details| details.get("seq"))
                                .and_then(serde_json::Value::as_u64)
                                == Some(seq.get())
                    })
                    .filter_map(|entry| {
                        entry
                            .get("details")
                            .and_then(|details| details.get("parts"))
                            .and_then(serde_json::Value::as_array)
                    })
                    .flatten()
                    .filter_map(|part| {
                        part.get("Adopt")
                            .and_then(|adopt| adopt.get("segment"))
                            .and_then(serde_json::Value::as_u64)
                    })
                    .collect();
                drop(observation);
                let [segment] = segments.as_slice() else {
                    return Err(format!(
                        "original control seek has {} native adopted segments",
                        segments.len()
                    ));
                };
                Ok((at, *segment))
            }
            _ => Err(format!(
                "original control seek has no unique applied outcome: {receipts:?}"
            )),
        }
    })();
    (
        trace,
        ControlCapture {
            pcm: captured,
            item: u64::from(id),
            rate,
            points,
            requested,
            landing,
            source: source_custody,
            decoded,
        },
    )
}

type DecodedChunk = (AudioChunkInfo, Arc<SampleBuffer>);
type SourcePosition = (u128, std::num::NonZeroU128);
type SampleResult<const N: usize> = Result<[f32; N], String>;

#[derive(Clone, Default)]
pub(in super::super) struct DecodedCapture {
    chunks: std::collections::BTreeMap<(u64, u64), DecodedChunk>,
    samples: usize,
    rejected: bool,
}

/// A captured chunk's source clock normalized to its own rate, with the raw
/// source extent a basis lookup may land in. The extent is widened by the
/// chunk's own width on both sides, so it only skips chunks whose exact
/// inverse cannot reach their frame range: the mapping is affine with a
/// positive step.
struct PhysicalCandidate<'a> {
    decoded: &'a AudioChunkInfo,
    basis: Result<(kithara::signal::SourceSpan, std::ops::RangeInclusive<f64>), String>,
}

impl<'a> PhysicalCandidate<'a> {
    fn new(decoded: &'a AudioChunkInfo) -> Self {
        let basis = decoded
            .source_geometry()
            .ok_or_else(|| "decoded PCM has no source geometry".to_owned())
            .and_then(|basis| {
                basis
                    .scaled_from(
                        (0, std::num::NonZeroU128::MIN),
                        (0, std::num::NonZeroU128::MIN),
                        (
                            u128::from(decoded.spec.sample_rate.get()),
                            std::num::NonZeroU128::from(basis.sample_rate()),
                        ),
                    )
                    .ok_or_else(|| "decoded PCM source clock cannot be normalized".to_owned())
            })
            .map(|basis| {
                let at = |frame| {
                    basis
                        .source_ratio_at(frame)
                        .map_or(f64::NAN, |(numerator, denominator)| {
                            numerator.to_f64().unwrap_or(f64::NAN)
                                / denominator.get().to_f64().unwrap_or(f64::NAN)
                        })
                };
                let (start, end) = (at(0), at(basis.output_frames()));
                let width = end - start + 1.0;
                let extent = if width.is_finite() {
                    start - width..=end + width
                } else {
                    f64::NEG_INFINITY..=f64::INFINITY
                };
                (basis, extent)
            });
        Self { decoded, basis }
    }
}

pub(in super::super) struct DecodedRecorder {
    incoming: HeapCons<Result<DecodedChunk, AudioObserveError>>,
    capture: DecodedCapture,
    chunk_limit: usize,
    sample_limit: usize,
}

impl DecodedRecorder {
    pub(in super::super) fn open() -> (Arc<Mutex<Self>>, DecodedObserver) {
        Self::with_limits(8_192, 16_777_216)
    }

    fn with_limits(chunks: usize, samples: usize) -> (Arc<Mutex<Self>>, DecodedObserver) {
        let (outgoing, incoming) = HeapRb::new(chunks + 1).split();
        let pools = pools_with_budget(samples * size_of::<f32>());
        (
            Arc::new(Mutex::new(Self {
                incoming,
                capture: DecodedCapture::default(),
                chunk_limit: chunks,
                sample_limit: samples,
            })),
            DecodedObserver {
                outgoing: Some(outgoing),
                pools,
            },
        )
    }

    pub(in super::super) fn view(&mut self) -> &DecodedCapture {
        while let Some(chunk) = self.incoming.try_pop() {
            let Ok((info, pcm)) = chunk else {
                self.capture.rejected = true;
                continue;
            };
            let key = (info.source_revision, info.frame_offset);
            if let Some((previous, samples)) = self.capture.chunks.get(&key) {
                self.capture.rejected |= *previous != info || samples[..] != pcm[..];
            } else if self.capture.chunks.len() >= self.chunk_limit
                || self.capture.samples + pcm.len() > self.sample_limit
            {
                self.capture.rejected = true;
            } else {
                self.capture.samples += pcm.len();
                self.capture.chunks.insert(key, (info, pcm));
            }
        }
        &self.capture
    }
}

pub(in super::super) struct DecodedObserver {
    outgoing: Option<HeapProd<Result<DecodedChunk, AudioObserveError>>>,
    pools: Pools,
}

impl DecodedObserver {
    fn reject(&mut self) -> Result<(), AudioObserveError> {
        if let Some(mut outgoing) = self.outgoing.take() {
            // Data offers always leave one slot for a sticky terminal failure.
            outgoing
                .try_push(Err(AudioObserveError::Full))
                .expect("reserved decoder failure slot");
        }
        Err(AudioObserveError::Full)
    }
}

impl AudioObserver for DecodedObserver {
    fn try_observe(&mut self, chunk: &AudioChunk) -> Result<(), AudioObserveError> {
        let outgoing = self.outgoing.as_ref().ok_or(AudioObserveError::Closed)?;
        if !outgoing.read_is_held() {
            self.outgoing = None;
            return Err(AudioObserveError::Closed);
        }
        let samples = chunk.as_ref();
        if outgoing.vacant_len() <= 1
            || samples.is_empty()
            || samples.len() > 32_768
            || samples.len() != chunk.frames() * usize::from(chunk.meta.spec.channels)
        {
            return self.reject();
        }
        let mut pcm = self.pools.get::<f32>();
        if pcm.try_extend_from_slice(samples).is_err() {
            return self.reject();
        }
        self.outgoing
            .as_mut()
            .expect("active decoder intake")
            .try_push(Ok((chunk.meta, Arc::new(pcm))))
            .expect("data slot checked before bounded PCM copy");
        Ok(())
    }
}

impl DecodedCapture {
    fn sampler(
        &self,
        info: consumed::SourceIdentity,
    ) -> impl FnMut(i64, usize) -> Result<f32, String> + '_ {
        let mut covering = None::<(&DecodedChunk, u64)>;
        move |frame, channel| {
            let frame = u64::try_from(frame).map_err(|error| error.to_string())?;
            let known = covering.filter(|(chunk, _)| chunk.0.frame_range().contains(&frame));
            let known = if let Some((chunk, mut end)) = known {
                if frame >= end {
                    end = self
                        .chunks
                        .range((
                            std::ops::Bound::Excluded((info.source_revision, end - 1)),
                            std::ops::Bound::Unbounded,
                        ))
                        .next()
                        .filter(|((revision, _), _)| *revision == info.source_revision)
                        .map_or_else(
                            || chunk.0.frame_range().end,
                            |((_, start), _)| (*start).min(chunk.0.frame_range().end),
                        );
                }
                (frame < end).then_some((chunk, end))
            } else {
                None
            };
            let (chunk, end) = if let Some(known) = known {
                known
            } else {
                // A single read proves coverage only through its authoritative floor lookup.
                (self.chunk(info, frame)?, frame + 1)
            };
            covering = Some((chunk, end));
            Self::read(chunk, frame, channel)
        }
    }

    fn sample(
        &self,
        info: consumed::SourceIdentity,
        frame: i64,
        channel: usize,
    ) -> Result<f32, String> {
        let frame = u64::try_from(frame).map_err(|error| error.to_string())?;
        Self::read(self.chunk(info, frame)?, frame, channel)
    }

    /// The accepted decoded chunk covering `frame` of the identified source.
    fn chunk(&self, info: consumed::SourceIdentity, frame: u64) -> Result<&DecodedChunk, String> {
        let (_, chunk) = self
            .chunks
            .range(..=(info.source_revision, frame))
            .next_back()
            .ok_or_else(|| format!("decoder observer has no PCM at frame {frame}"))?;
        let (decoded, _) = chunk;
        if decoded.source_revision != info.source_revision
            || decoded.spec != info.spec
            || decoded.media_origin != info.media_origin
            || !decoded.frame_range().contains(&frame)
        {
            let covering: Vec<_> = self
                .chunks
                .values()
                .filter(|(chunk, _)| {
                    chunk.source_revision == info.source_revision
                        && chunk.frame_range().contains(&frame)
                })
                .map(|(chunk, _)| *chunk)
                .take(4)
                .collect();
            let before: Vec<_> = self
                .chunks
                .range(..=(info.source_revision, frame))
                .rev()
                .take(3)
                .map(|(_, (chunk, _))| *chunk)
                .collect();
            let after: Vec<_> = self
                .chunks
                .range((info.source_revision, frame)..)
                .take(3)
                .map(|(_, (chunk, _))| *chunk)
                .collect();
            return Err(format!(
                "decoder observer has a revision/spec/origin/coverage hole at frame {frame}; requested={info:?}; selected={decoded:?}; covering={covering:?}; before={before:?}; after={after:?}; chunks={}; samples={}",
                self.chunks.len(),
                self.samples,
            ));
        }
        Ok(chunk)
    }

    fn read((decoded, samples): &DecodedChunk, frame: u64, channel: usize) -> Result<f32, String> {
        let offset =
            usize::try_from(frame - decoded.frame_offset).map_err(|error| error.to_string())?;
        samples
            .get(offset * usize::from(decoded.spec.channels) + channel)
            .copied()
            .filter(|sample| sample.is_finite())
            .ok_or_else(|| {
                format!("decoder observer has no finite PCM at frame {frame}, channel {channel}")
            })
    }

    pub(in super::super) fn physical_frame_basis(
        &self,
        info: impl Into<consumed::SourceIdentity>,
        frame: u64,
    ) -> Result<(u64, kithara::signal::SourceSpan), String> {
        let info = info.into();
        let (_, (decoded, _)) = self
            .chunks
            .range(..=(info.source_revision, frame))
            .next_back()
            .filter(|(_, (decoded, _))| {
                decoded.source_revision == info.source_revision
                    && decoded.spec == info.spec
                    && decoded.media_origin == info.media_origin
                    && decoded.frame_range().contains(&frame)
            })
            .ok_or_else(|| format!("decoder observer has no current physical frame {frame}"))?;
        let position = kithara::signal::SourceSpan::new(
            frame,
            frame
                .checked_add(1)
                .ok_or_else(|| "decoder physical frame overflows".to_owned())?,
            info.spec.sample_rate,
            1,
        )
        .and_then(|span| {
            decoded
                .source_geometry()?
                .map_output_span(decoded.frame_offset, span)
        })
        .and_then(|span| span.source_ratio_at(0))
        .ok_or_else(|| "decoder physical frame has no source coordinate".to_owned())?;
        self.physical_basis(info, position)
    }

    pub(in super::super) fn physical_basis(
        &self,
        info: impl Into<consumed::SourceIdentity>,
        position: (u128, std::num::NonZeroU128),
    ) -> Result<(u64, kithara::signal::SourceSpan), String> {
        let info = info.into();
        self.physical_basis_among(
            self.chunks
                .values()
                .filter(|(decoded, _)| info.describes(decoded))
                .map(|(decoded, _)| PhysicalCandidate::new(decoded)),
            info,
            position,
        )
    }

    /// Every captured chunk as a basis candidate, for a batch of lookups that
    /// would otherwise normalize the same chunks once per lookup.
    fn physical_candidates(&self) -> Vec<PhysicalCandidate<'_>> {
        self.chunks
            .values()
            .map(|(decoded, _)| PhysicalCandidate::new(decoded))
            .collect()
    }

    fn physical_basis_among<'a>(
        &self,
        candidates: impl IntoIterator<Item = impl std::borrow::Borrow<PhysicalCandidate<'a>>>,
        info: consumed::SourceIdentity,
        position: (u128, std::num::NonZeroU128),
    ) -> Result<(u64, kithara::signal::SourceSpan), String> {
        let raw =
            position.0.to_f64().unwrap_or(f64::NAN) / position.1.get().to_f64().unwrap_or(f64::NAN);
        for candidate in candidates {
            let candidate = candidate.borrow();
            let decoded = candidate.decoded;
            if !info.describes(decoded) {
                continue;
            }
            let (basis, extent) = candidate.basis.as_ref().map_err(Clone::clone)?;
            if !extent.contains(&raw) {
                continue;
            }
            let Some((frame, denominator)) =
                basis.output_ratio_at_source(decoded.frame_offset, position)
            else {
                continue;
            };
            let frame =
                u64::try_from(frame / denominator.get()).map_err(|error| error.to_string())?;
            if decoded.frame_range().contains(&frame) {
                return Ok((decoded.frame_offset, *basis));
            }
        }
        let mut identities = std::collections::BTreeMap::new();
        for (decoded, _) in self.chunks.values() {
            let key = (
                decoded.spec.sample_rate.get(),
                decoded.spec.channels,
                decoded.source_revision,
                decoded.media_origin,
            );
            let extent = identities.entry(key).or_insert_with(|| {
                (
                    0_usize,
                    decoded.frame_offset,
                    decoded.frame_range().end,
                    *decoded,
                    *decoded,
                )
            });
            extent.0 += 1;
            extent.1 = extent.1.min(decoded.frame_offset);
            extent.2 = extent.2.max(decoded.frame_range().end);
            extent.4 = *decoded;
        }
        Err(format!(
            "decoder observer has no physical basis for the raw source position; \
             requested={info:?}; position={position:?}; rejected={}; \
             captured_samples={}; identities={identities:?}",
            self.rejected, self.samples,
        ))
    }

    pub(in super::super) fn sample_at(
        &self,
        info: consumed::SourceIdentity,
        position: (u128, std::num::NonZeroU128),
        speed: f64,
        channel: usize,
    ) -> Result<f32, String> {
        self.samples_at(info, position, speed, [channel])
            .map(|[sample]| sample)
    }

    fn samples_at<const N: usize>(
        &self,
        info: consumed::SourceIdentity,
        position: SourcePosition,
        speed: f64,
        channels: [usize; N],
    ) -> SampleResult<N> {
        let kernel = sampling::SampleKernel::new((position.0, position.1.get()), speed)?;
        self.samples_with_kernel(position, &kernel, channels, &mut self.sampler(info))
    }

    fn samples_with_kernel<const N: usize>(
        &self,
        position: SourcePosition,
        kernel: &sampling::SampleKernel,
        channels: [usize; N],
        read: &mut impl FnMut(i64, usize) -> Result<f32, String>,
    ) -> SampleResult<N> {
        if self.rejected {
            return Err("decoder observer rejected or conflicted while capturing PCM".to_owned());
        }
        let source =
            i64::try_from(position.0 / position.1.get()).map_err(|error| error.to_string())?;
        kernel.samples(|offset| {
            let frame = source
                .checked_add(offset)
                .ok_or_else(|| "physical sample coordinate overflows".to_owned())?;
            let mut samples = [0.0; N];
            for (sample, channel) in samples.iter_mut().zip(channels) {
                *sample = read(frame, channel)?;
            }
            Ok(samples)
        })
    }

    pub(in super::super) fn fixture_domain(
        &self,
        info: AudioChunkInfo,
        reference: &Audio,
        witnessed_first: u64,
    ) -> Result<std::ops::Range<u64>, String> {
        if self.rejected
            || reference.channels != info.spec.channels
            || reference.sample_rate != info.spec.sample_rate.get()
            || info.media_origin != Some(Duration::ZERO)
        {
            return Err("source fixture has no exact native PCM identity/spec/origin".to_owned());
        }
        let channels = usize::from(reference.channels);
        let end =
            u64::try_from(reference.samples.len() / channels).map_err(|error| error.to_string())?;
        let declared_end = info
            .media_end
            .ok_or_else(|| "source has no actual declared EOF".to_owned())?;
        if info
            .spec
            .duration_for(end)
            .map_err(|error| error.to_string())?
            != declared_end
        {
            return Err("source EOF differs from the independently decoded fixture".to_owned());
        }
        let mut witnessed = false;
        let mut frontier = witnessed_first;
        for (chunk, samples) in self.chunks.values().filter(|(chunk, _)| {
            chunk.source_revision == info.source_revision
                && chunk.spec == info.spec
                && chunk.media_origin == info.media_origin
        }) {
            let range = chunk.frame_range();
            let geometry = chunk
                .source_geometry()
                .filter(|geometry| geometry.is_affine())
                .ok_or_else(|| "named fixture has no native affine source geometry".to_owned())?;
            for (offset, expected) in [(0, range.start), (u64::from(chunk.frames), range.end)] {
                let (numerator, denominator) = geometry
                    .source_ratio_at(offset)
                    .ok_or_else(|| "named fixture has invalid source geometry".to_owned())?;
                if numerator != u128::from(expected) * denominator.get()
                    || geometry.sample_rate() != chunk.spec.sample_rate
                {
                    return Err(
                        "named fixture source clock differs from its native PCM axis".to_owned(),
                    );
                }
            }
            witnessed |= range.start == witnessed_first;
            if range.end > witnessed_first {
                if range.start > frontier {
                    return Err(
                        "fixture identity has an interior decoded PCM hole after its witnessed origin"
                            .to_owned(),
                    );
                }
                frontier = frontier.max(range.end);
            }
            let begin = usize::try_from(range.start).map_err(|error| error.to_string())? * channels;
            let through = usize::try_from(range.end).map_err(|error| error.to_string())? * channels;
            let expected = reference.samples.get(begin..through).ok_or_else(|| {
                "captured source extends past the named native fixture".to_owned()
            })?;
            compare_pcm(samples, expected)?;
        }
        if !witnessed || frontier <= witnessed_first || frontier > end {
            return Err(
                "actual decoded source does not cover its witnessed native fixture origin"
                    .to_owned(),
            );
        }
        Ok(witnessed_first..end)
    }

    pub(in super::super) fn samples_at_domain<const N: usize>(
        &self,
        info: consumed::SourceIdentity,
        position: SourcePosition,
        speed: f64,
        channels: [usize; N],
        domain: &std::ops::Range<u64>,
    ) -> SampleResult<N> {
        let mut sample = self.sampler_at_domain(info, channels, domain.clone())?;
        sample(position, speed)
    }

    pub(in super::super) fn sampler_at_domain<const N: usize>(
        &self,
        info: consumed::SourceIdentity,
        channels: [usize; N],
        domain: std::ops::Range<u64>,
    ) -> Result<impl FnMut(SourcePosition, f64) -> SampleResult<N> + '_, String> {
        if self.rejected || domain.is_empty() {
            return Err("bounded fixture sampler has no accepted source domain".to_owned());
        }
        let first = i64::try_from(domain.start).map_err(|error| error.to_string())?;
        let last = i64::try_from(domain.end - 1).map_err(|error| error.to_string())?;
        let mut read = self.sampler(info);
        Ok(move |position: SourcePosition, speed: f64| {
            let source =
                i64::try_from(position.0 / position.1.get()).map_err(|error| error.to_string())?;
            mapped_samples((position.0, position.1.get()), speed, |offset| {
                let frame = source
                    .checked_add(offset)
                    .ok_or_else(|| "fixture sample coordinate overflows".to_owned())?;
                let mut samples = [0.0; N];
                for (sample, channel) in samples.iter_mut().zip(channels) {
                    *sample = read(frame.clamp(first, last), channel)?;
                }
                Ok(samples)
            })
        })
    }

    fn physical_capture_clock(
        &self,
        pcm: &[f32],
        marks: &[(SessionFrame, kithara_render::bridge::SlotSnapshot)],
    ) -> Result<(SessionFrame, usize), String> {
        if self.rejected {
            return Err(
                "decoder observer rejected PCM or saw conflicting source chunks".to_owned(),
            );
        }
        let &(capture_start, _) = marks
            .first()
            .ok_or_else(|| "physical capture has no initial mark".to_owned())?;
        let frames = pcm.len() / usize::from(CHANNELS);
        if marks
            .last()
            .is_none_or(|(frame, _)| i64::from(*frame) != i64::from(capture_start) + frames as i64)
        {
            return Err(
                "physical capture ledger does not end at the retained PCM boundary".to_owned(),
            );
        }
        Ok((capture_start, frames))
    }

    /// The affine inverse of the decoded basis holding raw `position`, with the
    /// physical PCM position it assigns to `position`.
    fn physical_inverse(
        &self,
        candidates: &[PhysicalCandidate<'_>],
        info: consumed::SourceIdentity,
        position: (u128, std::num::NonZeroU128),
    ) -> Result<
        (
            kithara::signal::SourceInverse,
            (u128, std::num::NonZeroU128),
        ),
        String,
    > {
        let (origin, basis) = self.physical_basis_among(candidates, info, position)?;
        let inverse = basis
            .source_inverse(origin)
            .ok_or_else(|| "physical basis has no affine inverse".to_owned())?;
        let first = inverse
            .output_at(position)
            .ok_or_else(|| "wrong-tempo negative has no physical origin".to_owned())?;
        Ok((inverse, first))
    }

    pub(in super::super) fn check_physical(
        &self,
        pcm: &[f32],
        marks: &[(SessionFrame, kithara_render::bridge::SlotSnapshot)],
        consumed: &consumed::Capture,
        grid: &BeatGridModel,
        wrong_source: &Audio,
        jump: Option<(SessionFrame, SessionFrame)>,
    ) -> Result<Vec<f32>, String> {
        let (capture_start, frames) = self.physical_capture_clock(pcm, marks)?;
        let sample_rate = consumed.sample_rate;
        check_physical_marks(marks, sample_rate)?;
        let intervals = physical_intervals(capture_start, frames, consumed, jump)?;
        let candidates = self.physical_candidates();
        let mut physical_bases = intervals
            .iter()
            .map(|(&start, interval)| {
                let info = interval.identity;
                let position = interval
                    .source
                    .source_ratio_at(0)
                    .ok_or_else(|| "physical source interval has no raw origin".to_owned())?;
                let (inverse, first) = self.physical_inverse(&candidates, info, position)?;
                let samplers = [self.sampler(info), self.sampler(info), self.sampler(info)];
                Ok((start, (inverse, first, samplers)))
            })
            .collect::<Result<std::collections::BTreeMap<_, _>, String>>()?;
        let mut reference = PhysicalReference {
            expected: Vec::with_capacity(pcm.len()),
            wrong_cue: Vec::with_capacity(pcm.len()),
            wrong_tempo: Vec::with_capacity(pcm.len()),
            wrong_payload: Vec::with_capacity(pcm.len()),
            supplied_beats: Vec::new(),
        };
        let mut next_position = None;
        for frame in 0..frames {
            let session = i64::from(capture_start) + frame as i64;
            let gain = physical_jump_gain(SessionFrame::new(session), jump);
            if !gain.is_finite() || !(0.0..=1.0).contains(&gain) {
                return Err("physical capture has an invalid commanded lane gain".to_owned());
            }
            let (&start, interval) = intervals.range(..=session).next_back().ok_or_else(|| {
                format!("physical capture has an uncovered output frame {session}")
            })?;
            let offset = u64::try_from(session - start).map_err(|error| error.to_string())?;
            let info = interval.identity;
            let span = interval.source;
            if offset >= span.output_frames() {
                let boundary = marks.partition_point(|(at, _)| i64::from(*at) <= session);
                let nearby = &marks[boundary.saturating_sub(2)..(boundary + 2).min(marks.len())];
                return Err(format!(
                    "physical capture has a SourceSpan hole at output frame {session}; \
                     capture_start={capture_start:?}; retained_frames={frames}; \
                     sample_rate={sample_rate}; interval_start={start}; \
                     interval={info:?}; nearby_points={nearby:?}"
                ));
            }
            let raw_next = span
                .source_ratio_at(offset + 1)
                .ok_or_else(|| "physical capture has an unmapped source endpoint".to_owned())?;
            let (inverse, first, samplers) = physical_bases
                .get_mut(&start)
                .ok_or_else(|| "physical source interval lost its decoded basis".to_owned())?;
            let (inverse, first) = (*inverse, *first);
            let [expected_source, cue_source, tempo_source] = samplers;
            let (raw_position, (numerator, denominator)) = if let Some((_, raw, physical)) =
                next_position.filter(|(previous_start, _, _)| *previous_start == start)
            {
                (raw, physical)
            } else {
                let raw = span
                    .source_ratio_at(offset)
                    .ok_or_else(|| "physical capture has an unmapped source frame".to_owned())?;
                let physical = inverse.output_at(raw).ok_or_else(|| {
                    "raw source coordinate has no physical PCM position".to_owned()
                })?;
                (raw, physical)
            };
            if offset == 0 && (numerator, denominator) != first {
                return Err(
                    "physical source interval origin differs from its captured basis".to_owned(),
                );
            }
            let physical_next = inverse
                .output_at(raw_next)
                .ok_or_else(|| "raw source endpoint has no physical PCM position".to_owned())?;
            next_position = Some((start, raw_next, physical_next));
            let speed = physical_source_speed((numerator, denominator), physical_next)?;
            let kernel = sampling::SampleKernel::new((numerator, denominator.get()), speed)?;
            let expected = self.samples_with_kernel(
                (numerator, denominator),
                &kernel,
                [0, 1],
                expected_source,
            )?;
            let wrong_position = numerator
                .checked_add(denominator.get())
                .ok_or_else(|| "wrong-cue negative coordinate overflows".to_owned())?;
            let wrong_cue = self
                .samples_with_kernel((wrong_position, denominator), &kernel, [0, 1], cue_source)
                .map_err(|error| format!("wrong-cue negative: {error}"))?;
            let wrong_denominator = denominator
                .get()
                .checked_mul(first.1.get())
                .and_then(|value| value.checked_mul(2))
                .and_then(std::num::NonZeroU128::new)
                .ok_or_else(|| "wrong-tempo negative denominator overflows".to_owned())?;
            let wrong_numerator = numerator
                .checked_mul(first.1.get())
                .and_then(|value| value.checked_add(first.0.checked_mul(denominator.get())?))
                .ok_or_else(|| "wrong-tempo negative coordinate overflows".to_owned())?;
            let wrong_tempo = sampling::SampleKernel::new(
                (wrong_numerator, wrong_denominator.get()),
                speed * 0.5,
            )
            .and_then(|kernel| {
                self.samples_with_kernel(
                    (wrong_numerator, wrong_denominator),
                    &kernel,
                    [0, 1],
                    tempo_source,
                )
            })
            .map_err(|error| format!("wrong-tempo negative: {error}"))?;
            for (channel, sample) in expected.into_iter().enumerate() {
                reference
                    .expected
                    .push(output_sample(sample * gain * CONTROL_GAIN).map_err(|_| {
                        format!("physical reference has no limiter headroom at frame {session}")
                    })?);
                reference
                    .wrong_cue
                    .push(output_sample(wrong_cue[channel] * gain * CONTROL_GAIN)?);
                reference
                    .wrong_tempo
                    .push(output_sample(wrong_tempo[channel] * gain * CONTROL_GAIN)?);
                let wrong_frame = (frame as u128 * u128::from(wrong_source.sample_rate))
                    / u128::from(sample_rate);
                reference.wrong_payload.push(output_sample(
                    source_sample(wrong_source, (wrong_frame, 1), 1.0, channel)?
                        * gain
                        * CONTROL_GAIN,
                )?);
            }
            physical_supplied_beats(
                &mut reference.supplied_beats,
                info,
                raw_position,
                raw_next,
                grid,
                sample_rate,
                frame,
            )?;
        }
        reference.check(pcm, sample_rate, frames)
    }
}

fn physical_intervals(
    capture_start: SessionFrame,
    frames: usize,
    consumed: &consumed::Capture,
    jump: Option<(SessionFrame, SessionFrame)>,
) -> Result<std::collections::BTreeMap<i64, consumed::Interval>, String> {
    let capture_end = i64::from(capture_start)
        .checked_add(i64::try_from(frames).map_err(|error| error.to_string())?)
        .ok_or_else(|| "physical capture endpoint overflows".to_owned())?;
    let intervals = consumed.covered(i64::from(capture_start)..capture_end)?;
    if let Some((_, landing)) = jump
        && !intervals.contains_key(&i64::from(landing))
    {
        return Err("commanded Jump landing has no actual source interval".to_owned());
    }
    Ok(intervals)
}

fn physical_supplied_beats(
    supplied_beats: &mut Vec<(i64, usize)>,
    info: consumed::SourceIdentity,
    raw_position: (u128, std::num::NonZeroU128),
    raw_next: (u128, std::num::NonZeroU128),
    grid: &BeatGridModel,
    sample_rate: u32,
    frame: usize,
) -> Result<(), String> {
    let media_origin = info
        .media_origin
        .ok_or_else(|| "physical capture lost media zero".to_owned())?;
    let source_start = raw_position
        .0
        .to_f64()
        .zip(raw_position.1.get().to_f64())
        .map(|(n, d)| n / d)
        .ok_or_else(|| "supplied beat has no source coordinate".to_owned())?;
    let source_end = raw_next
        .0
        .to_f64()
        .zip(raw_next.1.get().to_f64())
        .map(|(n, d)| n / d)
        .ok_or_else(|| "supplied beat has no source endpoint".to_owned())?;
    let beats = &grid.as_raw().beats;
    let first = beats.partition_point(|beat| {
        (beat.at + media_origin.as_secs_f64()) * f64::from(sample_rate) < source_start
    });
    for beat in &beats[first..] {
        if (beat.at + media_origin.as_secs_f64()) * f64::from(sample_rate) >= source_end {
            break;
        }
        supplied_beats.push((beat.ordinal, frame));
    }
    Ok(())
}

fn physical_jump_gain(frame: SessionFrame, jump: Option<(SessionFrame, SessionFrame)>) -> f32 {
    match jump {
        Some((start, landing)) if frame >= start => {
            let declick = (i64::from(landing) - i64::from(start))
                .to_f32()
                .expect("i64 declick frames fit the f32 range");
            let elapsed = (i64::from(frame) - i64::from(start))
                .to_f32()
                .expect("i64 elapsed frames fit the f32 range");
            if frame < landing {
                1.0 - (elapsed / declick.max(1.0)).min(1.0)
            } else {
                ((elapsed - declick) / declick.max(1.0)).min(1.0)
            }
        }
        _ => 1.0,
    }
}

pub(in super::super) fn physical_source_speed(
    (numerator, denominator): (u128, std::num::NonZeroU128),
    (next, next_denominator): (u128, std::num::NonZeroU128),
) -> Result<f64, String> {
    let step = next
        .checked_mul(denominator.get())
        .and_then(|next| next.checked_sub(numerator.checked_mul(next_denominator.get())?))
        .ok_or_else(|| "physical source advance overflows or reverses".to_owned())?;
    let step_denominator = next_denominator
        .get()
        .checked_mul(denominator.get())
        .ok_or_else(|| "physical source denominator overflows".to_owned())?;
    let mut divisor = step;
    let mut remainder = step_denominator;
    while remainder != 0 {
        (divisor, remainder) = (remainder, divisor % remainder);
    }
    let speed = (step / divisor)
        .to_f64()
        .zip((step_denominator / divisor).to_f64())
        .map(|(advance, period)| advance / period)
        .filter(|speed| speed.is_finite() && *speed > 0.0)
        .ok_or_else(|| "physical source speed cannot be represented".to_owned())?;
    Ok(speed)
}

fn check_physical_marks(
    marks: &[(SessionFrame, kithara_render::bridge::SlotSnapshot)],
    sample_rate: u32,
) -> Result<(), String> {
    for &(observed_at, slot) in marks {
        if slot.state != kithara_render::bridge::SlotState::Playing {
            continue;
        }
        let mark = slot
            .mark
            .ok_or_else(|| "Playing capture has no consumed mark".to_owned())?;
        if mark.session != observed_at {
            return Err(format!(
                "physical capture at {observed_at:?} has a stale consumed mark at {:?}",
                mark.session,
            ));
        }
        let info = slot
            .source
            .ok_or_else(|| "Playing capture has no source payload".to_owned())?;
        let span = info
            .source_span
            .ok_or_else(|| "Playing capture has no exact source span".to_owned())?;
        if mark.lane.segment != info.segment
            || slot.gain != 1.0
            || info.spec.sample_rate.get() != sample_rate
            || span.sample_rate().get() != sample_rate
            || info.spec.channels != CHANNELS
            || u64::from(info.frames) != span.output_frames()
            || info.media_origin.is_none()
        {
            return Err(
                "physical capture lost Segment, gain, sample-rate, channel or origin custody"
                    .to_owned(),
            );
        }
    }
    Ok(())
}

struct PhysicalReference {
    expected: Vec<f32>,
    wrong_cue: Vec<f32>,
    wrong_tempo: Vec<f32>,
    wrong_payload: Vec<f32>,
    supplied_beats: Vec<(i64, usize)>,
}

impl PhysicalReference {
    fn check(self, pcm: &[f32], sample_rate: u32, frames: usize) -> Result<Vec<f32>, String> {
        let Self {
            expected,
            wrong_cue,
            wrong_tempo,
            wrong_payload,
            supplied_beats,
        } = self;
        compare_pcm(pcm, &expected)?;
        let mut limited: Vec<Vec<f32>> = (0..usize::from(CHANNELS))
            .map(|channel| {
                expected
                    .chunks_exact(usize::from(CHANNELS))
                    .map(|frame| frame[channel])
                    .collect()
            })
            .collect();
        let mut limiter = kithara::effects::PeakLimiter::new(
            NonZeroU32::new(sample_rate)
                .ok_or_else(|| "physical capture has a zero output rate".to_owned())?,
            NonZeroUsize::new(usize::from(CHANNELS)).expect("fixture channels"),
            LimiterConfig::default(),
        )
        .map_err(|error| error.to_string())?;
        let mut channels: Vec<_> = limited.iter_mut().map(Vec::as_mut_slice).collect();
        limiter.process_planar(&mut channels);
        for (channel, limited) in limited.iter().enumerate() {
            let original: Vec<_> = expected
                .chunks_exact(usize::from(CHANNELS))
                .map(|frame| frame[channel])
                .collect();
            compare_pcm(limited, &original).map_err(|error| {
                format!("physical reference exceeds default limiter headroom: {error}")
            })?;
        }
        for (name, wrong) in [
            ("wrong source payload", wrong_payload),
            ("wrong source cue", wrong_cue),
            ("wrong tempo payload", wrong_tempo),
        ] {
            if compare_pcm(&wrong, &expected).is_ok() {
                return Err(format!(
                    "physical capture cannot reject {name} with unchanged consumed metadata"
                ));
            }
        }
        if !expected.iter().any(|sample| *sample != 0.0) {
            return Err("physical capture reference is entirely silent".to_owned());
        }
        let channels = usize::from(CHANNELS);
        if expected.len() < channels * 3 {
            return Err("physical capture has no one-frame negative domain".to_owned());
        }
        let interior = &expected[channels..expected.len() - channels];
        for shifted in [
            &expected[..expected.len() - channels * 2],
            &expected[channels * 2..],
        ] {
            if compare_pcm(shifted, interior).is_ok() {
                return Err("physical capture cannot reject a one-frame PCM shift".to_owned());
            }
        }
        for gain in [0.5, 1.0] {
            let wrong: Vec<_> = expected
                .iter()
                .map(|sample| sample * (gain / CONTROL_GAIN))
                .collect();
            if compare_pcm(&wrong, &expected).is_ok() {
                return Err("physical capture cannot reject wrong PCM gain".to_owned());
            }
        }
        if supplied_beats.is_empty() {
            return Err(
                "physical capture contains no crossing from the supplied beat grid".to_owned(),
            );
        }
        for (ordinal, frame) in supplied_beats {
            let start = frame.saturating_sub(64) * channels;
            let end = (frame + 64).min(frames) * channels;
            let mut missing = expected.clone();
            missing[start..end].fill(0.0);
            if compare_pcm(&missing, &expected).is_ok() {
                return Err(format!(
                    "physical capture cannot reject removed supplied beat {ordinal}"
                ));
            }
        }
        let mut shifted = pcm.to_vec();
        shifted.rotate_left(usize::from(CHANNELS));
        if compare_pcm(&shifted, &expected).is_ok() {
            return Err(
                "physical oracle accepted shifted PCM with unchanged native metadata".to_owned(),
            );
        }
        Ok(expected)
    }
}

#[kithara::test(native)]
fn output_reference_keeps_the_limiter_sample_domain_and_strict_pcm_checks() {
    let source = [
        0.0,
        -0.0,
        f32::from_bits(1),
        -f32::from_bits(1),
        f32::MIN_POSITIVE / 2.0,
        -f32::MIN_POSITIVE / 2.0,
        f32::MIN_POSITIVE,
        -f32::MIN_POSITIVE,
        0.125,
        -0.25,
    ];
    let mut actual = source;
    let mut limiter = kithara::effects::PeakLimiter::new(
        NonZeroU32::new(48_000).expect("output rate"),
        NonZeroUsize::MIN,
        LimiterConfig::default(),
    )
    .expect("output limiter");
    limiter.process_planar(&mut [&mut actual]);
    let expected: Vec<_> = source
        .into_iter()
        .map(output_sample)
        .collect::<Result<_, _>>()
        .expect("finite reference with headroom");
    compare_pcm(&actual, &expected).expect("reference matches the actual output contract");
    assert!(compare_pcm(&[f32::from_bits(1)], &[0.0]).is_err());
    let mut corrupted = actual;
    corrupted[8] = f32::from_bits(corrupted[8].to_bits() + 1);
    assert!(compare_pcm(&corrupted, &expected).is_err());
    for invalid in [
        f32::NAN,
        f32::INFINITY,
        f32::NEG_INFINITY,
        LimiterConfig::default().ceiling(),
    ] {
        assert!(output_sample(invalid).is_err());
    }
}

#[kithara::test(native)]
fn native_stereo_sampling_preserves_scalar_bits_and_coverage_errors() {
    let values = [0.0, -0.0, f32::from_bits(1), -0.125, 0.75, f32::MAX];
    let mut reference = Audio {
        samples: (0..256).map(|i| values[i % values.len()]).collect(),
        sample_rate: 48_000,
        channels: CHANNELS,
    };
    for invalid in [
        None,
        Some(f32::NAN),
        Some(f32::INFINITY),
        Some(f32::NEG_INFINITY),
    ] {
        reference.samples[64] = invalid.unwrap_or(values[64 % values.len()]);
        for position in [(0, 1), (65, 2), (129, 4), (128, 3), (127, 1), (128, 1)] {
            for speed in [0.5_f32, 1.0, 1.25, 1.5, 8.0] {
                let source = i64::try_from(position.0 / position.1).unwrap();
                let mut expected = [0; 2];
                let mut error = None;
                for (channel, expected) in expected.iter_mut().enumerate() {
                    let scalar = sampling::scalar_reference(position, f64::from(speed), |offset| {
                        source
                            .checked_add(offset)
                            .and_then(|frame| usize::try_from(frame).ok())
                            .and_then(|frame| frame.checked_mul(usize::from(reference.channels)))
                            .and_then(|index| index.checked_add(channel))
                            .and_then(|index| reference.samples.get(index))
                            .copied()
                            .filter(|sample| sample.is_finite())
                            .ok_or_else(|| format!("native source does not cover frame {source}+{offset}, channel {channel}"))
                    });
                    match scalar {
                        Ok(sample) => *expected = sample.to_bits(),
                        Err(found) => {
                            error = Some(found);
                            break;
                        }
                    }
                }
                let actual = source_samples(&reference, position, speed, [0, 1]);
                match (actual, error) {
                    (Ok(actual), None) => assert_eq!(actual.map(f32::to_bits), expected),
                    (Err(_), Some(_)) => {}
                    (actual, error) => panic!(
                        "native sampling differs for {position:?} at {speed}: {actual:?}, {error:?}"
                    ),
                }
            }
        }
    }
}

fn check_persistent_physical_sampling(
    capture: &DecodedCapture,
    identity: consumed::SourceIdentity,
) {
    let mut readers = [
        capture.sampler(identity),
        capture.sampler(identity),
        capture.sampler(identity),
    ];
    for speed in [0.5, 1.0, 1.25, 1.5, 8.0, f64::INFINITY, f64::NAN] {
        for (frame, phase) in [
            (0_u128, 0),
            (30, 1),
            (31, 3),
            (32, 0),
            (63, 1),
            (64, 3),
            (95, 1),
            (63, 0),
            (0, 1),
            (u128::MAX / 4 - 2, 3),
        ] {
            let numerator = frame * 4 + phase;
            let positions = [(numerator, 4), (numerator + 4, 4), (numerator, 8)];
            for ((read, (numerator, denominator)), speed) in readers
                .iter_mut()
                .zip(positions)
                .zip([speed, speed, speed * 0.5])
            {
                let denominator =
                    std::num::NonZeroU128::new(denominator).expect("physical reference phase");
                let position = (numerator, denominator);
                let kernel = sampling::SampleKernel::new((numerator, denominator.get()), speed)
                    .expect("representable physical reference phase");
                let mut actual_reads = Vec::new();
                let actual = capture.samples_with_kernel(
                    position,
                    &kernel,
                    [0, 1],
                    &mut |frame, channel| {
                        actual_reads.push((frame, channel));
                        read(frame, channel)
                    },
                );
                let mut expected_reads = Vec::new();
                let expected = capture.samples_with_kernel(
                    position,
                    &kernel,
                    [0, 1],
                    &mut |frame, channel| {
                        expected_reads.push((frame, channel));
                        capture.sample(identity, frame, channel)
                    },
                );
                assert_eq!(actual_reads, expected_reads, "{position:?}, {speed}");
                let bits = |result: SampleResult<2>| result.map(|pcm| pcm.map(f32::to_bits));
                assert_eq!(bits(actual), bits(expected), "{position:?}, {speed}");
                assert_eq!(
                    bits(capture.samples_at(identity, position, speed, [0, 1])),
                    bits(capture.samples_with_kernel(position, &kernel, [0, 1], read)),
                    "fresh and retained cursors: {position:?}, {speed}",
                );
            }
        }
    }
}

#[kithara::test(native)]
fn bounded_sinc_reads_preserve_chunk_edges_and_source_identity() {
    let asset = kithara_test_fixtures::assets::by_name("sine_wav_a440_10000_frames")
        .expect("whole-track PCM fixture");
    let reference = kithara_integration_tests::pcm_oracle::prepared_native(&asset)
        .expect("build-time independent native PCM");
    let spec = AudioSpec::new(
        reference.channels,
        NonZeroU32::new(reference.sample_rate).expect("fixture sample rate"),
    );
    let mut capture = DecodedCapture::default();
    let pcm_pools = pools();
    for start in [0_u64, 32, 64] {
        let info = AudioChunkInfo {
            spec,
            source_revision: 7,
            frame_offset: start,
            frames: 32,
            timestamp: spec.duration_for(start).expect("chunk start"),
            end_timestamp: spec.duration_for(start + 32).expect("chunk end"),
            media_origin: Some(Duration::ZERO),
            ..AudioChunkInfo::default()
        };
        let start = usize::try_from(start).expect("fixture frame offset");
        let channels = usize::from(reference.channels);
        let samples = Arc::new(sample_buffer(
            &pcm_pools,
            &reference.samples[start * channels..(start + 32) * channels],
        ));
        capture.samples += samples.len();
        capture
            .chunks
            .insert((7, info.frame_offset), (info, samples));
    }
    let identity = consumed::SourceIdentity {
        spec,
        source_revision: 7,
        media_origin: Some(Duration::ZERO),
    };
    check_persistent_physical_sampling(&capture, identity);
    let mut rejected = capture.clone();
    rejected.rejected = true;
    check_persistent_physical_sampling(&rejected, identity);
    let mut missing_chunk = capture.clone();
    missing_chunk.chunks.remove(&(7, 32));
    check_persistent_physical_sampling(&missing_chunk, identity);
    let denominator = std::num::NonZeroU128::new(4).expect("quarter-frame phase");
    let domain = 0..96;
    let mut across_frames = capture
        .sampler_at_domain(identity, [0, 1], domain.clone())
        .expect("accepted persistent source domain");
    for (frame, speed) in [
        (0_i64, 1.25),
        (30, 0.5),
        (30, 1.0),
        (30, 1.5),
        (31, 1.75),
        (32, 1.25),
        (63, 1.5),
        (95, 1.25),
        (63, 0.5),
        (0, 1.25),
        (95, 1.0),
        (31, 1.75),
        (32, 1.75),
    ] {
        let numerator = u128::try_from(frame).expect("nonnegative frame") * 4 + 1;
        for channel in 0..usize::from(reference.channels) {
            let expected = mapped_sample((numerator, denominator.get()), speed, |offset| {
                capture.sample(identity, (frame + offset).clamp(0, 95), channel)
            })
            .expect("independent per-tap lookup");
            let actual = capture
                .samples_at_domain(
                    identity,
                    (numerator, denominator),
                    speed,
                    [channel],
                    &domain,
                )
                .expect("covering chunk lookup");
            assert_eq!(actual[0].to_bits(), expected.to_bits());
            let stereo = capture
                .samples_at_domain(identity, (numerator, denominator), speed, [0, 1], &domain)
                .expect("bounded stereo lookup");
            assert_eq!(stereo[channel].to_bits(), expected.to_bits());
            let persisted = across_frames((numerator, denominator), speed)
                .expect("source lookup across forward and backward phase windows");
            assert_eq!(persisted[channel].to_bits(), expected.to_bits());
            if (16..80).contains(&frame) {
                let physical = capture
                    .sample_at(identity, (numerator, denominator), speed, channel)
                    .expect("unbounded physical source lookup");
                assert_eq!(physical.to_bits(), expected.to_bits());
                let stereo = capture
                    .samples_at(identity, (numerator, denominator), speed, [0, 1])
                    .expect("physical stereo lookup");
                assert_eq!(stereo[channel].to_bits(), expected.to_bits());
            } else {
                assert!(
                    capture
                        .samples_at(identity, (numerator, denominator), speed, [0, 1])
                        .is_err()
                );
                assert!(
                    capture
                        .sample_at(identity, (numerator, denominator), speed, channel)
                        .is_err()
                );
            }
        }
    }
    let mut read = capture.sampler(identity);
    for frame in [31, 31, 33, 32, 79, 64, 63, 0, 95] {
        for channel in 0..usize::from(reference.channels) {
            assert_eq!(
                read(frame, channel)
                    .expect("cached source sample")
                    .to_bits(),
                capture
                    .sample(identity, frame, channel)
                    .expect("independent source sample")
                    .to_bits(),
            );
        }
    }
    let mut overlapping = capture.clone();
    let channels = usize::from(reference.channels);
    let first = overlapping
        .chunks
        .get_mut(&(7, 0))
        .expect("first source chunk");
    first.0.frames = 64;
    first.0.end_timestamp = spec.duration_for(64).expect("extended source end");
    first.1 = Arc::new(sample_buffer(
        &pcm_pools,
        &reference.samples[..64 * channels],
    ));
    let replacement = AudioChunkInfo {
        frame_offset: 31,
        frames: 32,
        timestamp: spec.duration_for(31).expect("overlapping source start"),
        end_timestamp: spec.duration_for(63).expect("overlapping source end"),
        ..first.0
    };
    overlapping.chunks.insert(
        (7, 31),
        (
            replacement,
            Arc::new(sample_buffer(
                &pcm_pools,
                &reference.samples[31 * channels..63 * channels]
                    .iter()
                    .map(|sample| sample * 0.5)
                    .collect::<Vec<_>>(),
            )),
        ),
    );
    overlapping.samples += 64 * channels;
    check_persistent_physical_sampling(&overlapping, identity);
    assert_ne!(
        overlapping
            .sample(identity, 31, 0)
            .expect("superseding PCM"),
        capture.sample(identity, 31, 0).expect("original PCM"),
    );
    let expected = mapped_sample((125, denominator.get()), 1.5, |offset| {
        overlapping.sample(identity, 31 + offset, 0)
    })
    .expect("latest-start overlapping source");
    assert_eq!(
        overlapping
            .sample_at(identity, (125, denominator), 1.5, 0)
            .expect("physical overlapping source")
            .to_bits(),
        expected.to_bits(),
    );
    {
        let mut read = overlapping.sampler(identity);
        for frame in [30, 31, 32, 62, 63, 64, 32, 30] {
            assert_eq!(
                read(frame, 0).expect("overlapping cached source").to_bits(),
                overlapping
                    .sample(identity, frame, 0)
                    .expect("latest authoritative source")
                    .to_bits(),
            );
        }
    }
    assert_eq!(
        overlapping
            .samples_at_domain(identity, (125, denominator), 1.5, [0], &domain)
            .expect("covering overlapping source")[0]
            .to_bits(),
        expected.to_bits(),
    );
    for channel in 0..usize::from(reference.channels) {
        let expected = mapped_sample((125, denominator.get()), 1.5, |offset| {
            overlapping.sample(identity, 31 + offset, channel)
        })
        .expect("authoritative overlapping channel");
        let stereo = overlapping
            .samples_at(identity, (125, denominator), 1.5, [0, 1])
            .expect("authoritative overlapping stereo");
        assert_eq!(stereo[channel].to_bits(), expected.to_bits());
        let bounded = overlapping
            .samples_at_domain(identity, (125, denominator), 1.5, [0, 1], &domain)
            .expect("bounded authoritative overlapping stereo");
        assert_eq!(bounded[channel].to_bits(), expected.to_bits());
    }
    overlapping
        .chunks
        .get_mut(&(7, 31))
        .expect("overlapping source chunk")
        .0
        .media_origin = Some(Duration::from_millis(1));
    check_persistent_physical_sampling(&overlapping, identity);
    {
        let mut invalid = overlapping
            .sampler_at_domain(identity, [0], domain.clone())
            .expect("declared domain remains nonempty");
        for frame in [30_u64, 31, 32, 20, 63, 30] {
            assert_eq!(
                invalid((u128::from(frame) * 4, denominator), 1.0).map(|[sample]| sample.to_bits()),
                overlapping
                    .sample(identity, frame.try_into().unwrap(), 0)
                    .map(f32::to_bits),
            );
        }
    }
    assert!(
        mapped_sample((125, denominator.get()), 1.5, |offset| {
            overlapping.sample(identity, 31 + offset, 0)
        })
        .is_err()
    );
    assert!(
        overlapping
            .samples_at_domain(identity, (125, denominator), 1.5, [0], &domain)
            .is_err()
    );
    assert!(
        overlapping
            .sample_at(identity, (125, denominator), 1.5, 0)
            .is_err()
    );
    let wrong_identity = consumed::SourceIdentity {
        source_revision: 8,
        ..identity
    };
    check_persistent_physical_sampling(&capture, wrong_identity);
    assert!(
        capture
            .samples_at(wrong_identity, (125, denominator), 1.5, [0, 1])
            .is_err()
    );
    assert!(
        capture
            .samples_at_domain(wrong_identity, (125, denominator), 1.5, [0], &domain)
            .is_err()
    );
    assert!(
        capture
            .sample_at(wrong_identity, (125, denominator), 1.5, 0)
            .is_err()
    );
    let wrong_identity = consumed::SourceIdentity {
        media_origin: Some(Duration::from_millis(1)),
        ..identity
    };
    check_persistent_physical_sampling(&capture, wrong_identity);
    assert!(
        capture
            .samples_at(wrong_identity, (125, denominator), 1.5, [0, 1])
            .is_err()
    );
    assert!(
        capture
            .samples_at_domain(wrong_identity, (125, denominator), 1.5, [0], &domain)
            .is_err()
    );
    let wrong_identity = consumed::SourceIdentity {
        spec: AudioSpec::new(1, spec.sample_rate),
        ..identity
    };
    check_persistent_physical_sampling(&capture, wrong_identity);
    assert!(
        capture
            .samples_at(wrong_identity, (125, denominator), 1.5, [0, 1])
            .is_err()
    );
    assert!(
        capture
            .samples_at_domain(wrong_identity, (125, denominator), 1.5, [0], &domain)
            .is_err()
    );
    assert!(
        capture
            .samples_at_domain(identity, (125, denominator), 1.5, [0], &(32..32))
            .is_err()
    );
    for channel in [0, 1] {
        for invalid in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let mut broken = capture.clone();
            let (_, pcm) = broken.chunks.get_mut(&(7, 32)).expect("middle chunk");
            let mut changed = sample_buffer(&pcm_pools, &pcm[..]);
            changed[channel] = invalid;
            *pcm = Arc::new(changed);
            check_persistent_physical_sampling(&broken, identity);
            for speed in [0.5, 1.5] {
                assert!(
                    broken
                        .sample_at(identity, (129, denominator), speed, channel)
                        .is_err()
                );
                assert!(
                    broken
                        .samples_at(identity, (129, denominator), speed, [0, 1])
                        .is_err()
                );
                assert!(
                    broken
                        .samples_at_domain(identity, (129, denominator), speed, [0, 1], &domain)
                        .is_err()
                );
            }
        }
    }
    let mut missing_right = capture.clone();
    let (_, pcm) = missing_right.chunks.get_mut(&(7, 64)).expect("last chunk");
    let truncated = sample_buffer(&pcm_pools, &pcm[..pcm.len() - 1]);
    *pcm = Arc::new(truncated);
    check_persistent_physical_sampling(&missing_right, identity);
    assert!(
        missing_right
            .sample_at(identity, (380, denominator), 1.0, 0)
            .is_ok()
    );
    assert!(
        missing_right
            .sample_at(identity, (380, denominator), 1.0, 1)
            .is_err()
    );
    assert!(
        missing_right
            .samples_at(identity, (380, denominator), 1.0, [0, 1])
            .is_err()
    );
    assert!(
        missing_right
            .samples_at_domain(identity, (380, denominator), 1.0, [0, 1], &domain)
            .is_err()
    );
}

#[kithara::test(native)]
fn decoded_capture_analysis_does_not_drop_the_next_chunk() {
    let (captured, mut observer) = DecodedRecorder::open();
    let mut analysis = captured.lock();
    let before = analysis.view().clone();
    let spec = AudioSpec::new(CHANNELS, NonZeroU32::new(48_000).expect("test sample rate"));
    let info = AudioChunkInfo {
        spec,
        frame_offset: 32,
        frames: 2,
        source_revision: 7,
        media_origin: Some(Duration::ZERO),
        ..AudioChunkInfo::default()
    };
    let pcm = [0.125, -0.25, 0.5, -0.75];
    let chunk = AudioChunk::new(info, sample_buffer(&pools(), &pcm));
    observer
        .try_observe(&chunk)
        .expect("analysis must not discard the next decoder chunk");
    assert!(before.chunks.is_empty());
    drop(analysis);
    let observed = captured.lock().view().clone();
    let (actual_info, actual_pcm) = observed.chunks.get(&(7, 32)).expect("accepted chunk");
    assert_eq!(*actual_info, info);
    assert_eq!(&actual_pcm[..], pcm);
    assert!(!observed.rejected);
}

fn decoded_test_chunk(frame_offset: u64, samples: &[f32]) -> AudioChunk {
    AudioChunk::new(
        AudioChunkInfo {
            spec: AudioSpec::new(CHANNELS, NonZeroU32::new(48_000).expect("test rate")),
            frame_offset,
            frames: u32::try_from(samples.len() / usize::from(CHANNELS)).expect("test frames"),
            source_revision: 7,
            media_origin: Some(Duration::ZERO),
            ..AudioChunkInfo::default()
        },
        sample_buffer(&pools(), samples),
    )
}

#[kithara::test(native)]
#[case::queue(1, 16)]
#[case::budget(8, 4)]
fn decoded_capture_saturation_is_sticky(#[case] chunks: usize, #[case] samples: usize) {
    let (recording, mut observer) = DecodedRecorder::with_limits(chunks, samples);
    let mut held = recording.lock();
    let first = decoded_test_chunk(0, &[0.125, -0.25, 0.5, -0.75]);
    observer.try_observe(&first).expect("first fact fits");
    let next = decoded_test_chunk(2, &[0.25, -0.5, 0.75, -1.0]);
    assert_eq!(observer.try_observe(&next), Err(AudioObserveError::Full));
    assert_eq!(observer.try_observe(&next), Err(AudioObserveError::Closed));
    let captured = held.view();
    assert!(captured.rejected);
    assert_eq!(captured.chunks.len(), 1);
    assert_eq!(captured.samples, 4);
    assert_eq!(&captured.chunks[&(7, 0)].1[..], first.as_ref());
    assert!(held.view().rejected, "draining does not erase the failure");
    drop(held);
}

#[kithara::test(native)]
#[case::empty(0)]
#[case::oversized(32_770)]
#[case::partial_frame(3)]
fn decoded_capture_malformed_facts_are_visible(#[case] len: usize) {
    let (recording, mut observer) = DecodedRecorder::open();
    let chunk = decoded_test_chunk(0, &vec![0.125; len]);
    assert_eq!(observer.try_observe(&chunk), Err(AudioObserveError::Full));
    let captured = recording.lock().view().clone();
    assert!(captured.rejected);
    assert!(captured.chunks.is_empty());
    assert_eq!(captured.samples, 0);
}

#[kithara::test(native)]
fn decoded_capture_bounds_unique_facts_without_counting_duplicates() {
    let (recording, mut observer) = DecodedRecorder::with_limits(2, 16);
    let first = decoded_test_chunk(0, &[0.125, -0.25, 0.5, -0.75]);
    for _ in 0..10 {
        observer
            .try_observe(&first)
            .expect("duplicate fits the handoff");
        let captured = recording.lock().view().clone();
        assert_eq!(captured.chunks.len(), 1);
        assert_eq!(captured.samples, 4);
        assert!(!captured.rejected);
    }
    let before = recording.lock().view().clone();
    assert_ne!(before.chunks[&(7, 0)].1.as_ptr(), first.as_ref().as_ptr());
    let second = decoded_test_chunk(2, &[0.25, -0.5, 0.75, -1.0]);
    observer.try_observe(&second).expect("second unique fact");
    let after = recording.lock().view().clone();
    assert_eq!(before.chunks.len(), 1, "old snapshot has no receiver");
    assert_eq!(after.chunks.len(), 2);
    assert_eq!(after.samples, 8);
    assert!(!after.rejected);
    assert!(Arc::ptr_eq(
        &before.chunks[&(7, 0)].1,
        &after.chunks[&(7, 0)].1
    ));
    assert_eq!(&before.chunks[&(7, 0)].1[..], first.as_ref());
    let third = decoded_test_chunk(4, &[0.5, -0.25, 0.75, -0.125]);
    observer.try_observe(&third).expect("handoff has room");
    let full = recording.lock().view().clone();
    assert!(
        full.rejected,
        "collector enforces the retained unique limit"
    );
    assert_eq!(full.chunks.len(), 2);
    assert_eq!(full.samples, 8);
    assert!(!before.rejected, "prior immutable snapshot is unchanged");
}

#[kithara::test(native)]
#[case::metadata(true)]
#[case::pcm(false)]
fn decoded_capture_conflicting_fact_is_visible(#[case] change_metadata: bool) {
    let (recording, mut observer) = DecodedRecorder::with_limits(2, 16);
    let first = decoded_test_chunk(0, &[0.125, -0.25, 0.5, -0.75]);
    observer.try_observe(&first).expect("original fact");
    let before = recording.lock().view().clone();
    let mut changed = decoded_test_chunk(0, first.as_ref());
    if change_metadata {
        changed.meta.media_origin = Some(Duration::from_millis(1));
    } else {
        changed.samples[0] += 0.125;
    }
    observer
        .try_observe(&changed)
        .expect("handoff accepts the fact");
    let after = recording.lock().view().clone();
    assert!(after.rejected);
    assert_eq!(after.chunks.len(), 1);
    assert_eq!(after.samples, 4);
    assert_eq!(after.chunks[&(7, 0)].0, first.meta);
    assert_eq!(&after.chunks[&(7, 0)].1[..], first.as_ref());
    assert!(!before.rejected);
}

#[kithara::test(native)]
fn decoded_capture_reader_drop_closes_observer() {
    let (recording, mut observer) = DecodedRecorder::open();
    let chunk = decoded_test_chunk(0, &[0.125, -0.25, 0.5, -0.75]);
    drop(recording);
    assert_eq!(observer.try_observe(&chunk), Err(AudioObserveError::Closed));
}
