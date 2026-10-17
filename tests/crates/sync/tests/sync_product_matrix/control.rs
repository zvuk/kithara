use kithara::{
    effects::LimiterConfig,
    play::{DeckMixSettings, Position, Settled},
};
use kithara_integration_tests::{
    offline::{OfflinePlayer, OfflinePlayerOptions, linked::LinkReceiptOrigin},
    pcm_oracle::Audio,
};
use kithara_render::bridge::DeckSnapshot;
use num_traits::ToPrimitive;

use super::{cases::*, imports::*};

pub(super) const CONTROL_GAIN: f32 = 0.25;
const CONTROL_CAPTURE_FRAMES: usize = 128;

pub(super) struct ControlCapture {
    pub(super) pcm: Vec<f32>,
    item: u64,
    rate: f32,
    points: Vec<ControlPoint>,
    requested: Position,
    landing: Result<(SessionFrame, u64), String>,
    source: Result<(), String>,
    decoded: Arc<Mutex<DecodedCapture>>,
}

#[derive(Clone)]
struct ControlPoint {
    frame: usize,
    rendered_frames: usize,
    session: u64,
    deck: DeckSnapshot,
}

impl ControlCapture {
    pub(super) fn evidence(&self) -> serde_json::Value {
        serde_json::json!({
            "rate": self.rate,
            "requested_mix_gain": CONTROL_GAIN,
            "requested_ns": self.requested.as_nanos().to_string(),
            "landing": format!("{:?}", self.landing),
            "requested_source": format!("{:?}", self.source),
            "decoded_chunks": self.decoded.lock().chunks.len(),
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

    pub(super) fn source_content_failures(
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

    pub(super) fn observed_content_failures(
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
        let consumed = super::consumed::Capture::for_track(self.item, first.deck.sample_rate)?;
        let decoded = self.decoded.lock();
        decoded.check_physical(&self.pcm, &marks, &consumed, grid, wrong_source, None)?;
        let mut shifted = self.pcm.clone();
        shifted.rotate_left(usize::from(CHANNELS));
        if decoded
            .check_physical(&shifted, &marks, &consumed, grid, wrong_source, None)
            .is_ok()
        {
            return Err(
                "physical oracle accepted shifted PCM with unchanged native metadata".to_owned(),
            );
        }
        drop(decoded);
        let retained = i64::from(marks.first().expect("capture has an initial mark").0)
            ..i64::from(marks.last().expect("capture has a final mark").0);
        super::consumed::check_trace_negatives(self.item, first.deck.sample_rate, retained)
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
        info: kithara::signal::AudioChunkInfo,
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
            for channel in 0..usize::from(CHANNELS) {
                expected
                    .push(source_sample(reference, position, self.rate, channel)? * CONTROL_GAIN);
            }
        }
        if expected
            .iter()
            .any(|sample| !sample.is_finite() || sample.abs() >= LimiterConfig::default().ceiling())
        {
            return Err("control reference exceeds its configured output headroom".to_owned());
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
    let source = i64::try_from(position.0 / position.1).map_err(|error| error.to_string())?;
    let sample = |offset: i64| {
        let frame = source
            .checked_add(offset)
            .and_then(|frame| usize::try_from(frame).ok());
        frame
            .and_then(|frame| frame.checked_mul(usize::from(reference.channels)))
            .and_then(|index| index.checked_add(channel))
            .and_then(|index| reference.samples.get(index))
            .copied()
            .filter(|sample| sample.is_finite())
            .ok_or_else(|| {
                format!("native source does not cover frame {source}+{offset}, channel {channel}")
            })
    };
    mapped_sample(position, f64::from(speed), sample)
}

fn mapped_sample(
    position: (u128, u128),
    speed: f64,
    sample: impl Fn(i64) -> Result<f32, String>,
) -> Result<f32, String> {
    let fraction = (position.0 % position.1)
        .to_f64()
        .zip(position.1.to_f64())
        .map(|(remainder, denominator)| remainder / denominator)
        .ok_or_else(|| "source fraction is not representable".to_owned())?;
    if speed <= 1.0 {
        if fraction == 0.0 {
            return sample(0);
        }
        let current = f64::from(sample(0)?);
        let next = f64::from(sample(1)?);
        return (next - current)
            .mul_add(fraction, current)
            .to_f32()
            .filter(|sample| sample.is_finite())
            .ok_or_else(|| "control source sample is not finite f32".to_owned());
    }
    let cutoff = speed.recip();
    let mut total = 0.0;
    let mut weights = 0.0;
    for offset in -16_i32..=16 {
        let distance = f64::from(offset) - fraction;
        let weight = kithara::dsp::interp::sinc_hann_weight(distance, cutoff, 17.0);
        total += f64::from(sample(i64::from(offset))?) * weight;
        weights += weight;
    }
    (total / weights)
        .to_f32()
        .filter(|sample| sample.is_finite())
        .ok_or_else(|| "control source sample is not finite f32".to_owned())
}

pub(super) fn compare_pcm(actual: &[f32], expected: &[f32]) -> Result<(), String> {
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
            for channel in 0..channels {
                wrong
                    .push(source_sample(reference, position, wrong_speed, channel)? * CONTROL_GAIN);
            }
        }
        if compare_pcm(&wrong, &expected[..window * channels]).is_ok() {
            return Err(format!(
                "control source cannot reject wrong PCM scalar {wrong_speed}"
            ));
        }
    }
    let window = frames.min(BLOCK_FRAMES);
    for (name, source, cue_offset) in [
        ("wrong cue", reference, rate.1),
        ("wrong source", wrong_source, 0),
    ] {
        if source.sample_rate != reference.sample_rate || source.channels != reference.channels {
            return Err(format!(
                "{name} negative fixture has a different native spec"
            ));
        }
        let mut wrong = Vec::with_capacity(window * channels);
        for frame in 0..window {
            let position = (origin + cue_offset + frame as u128 * rate.0, rate.1);
            for channel in 0..channels {
                wrong.push(source_sample(source, position, speed, channel)? * CONTROL_GAIN);
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
pub(super) async fn capture(
    source: String,
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
        .store(memory_asset_store())
        .initial_abr_mode(AbrMode::manual(0))
        .build();
    let id = harness.load_config(config).await;
    let decoded = Arc::new(Mutex::new(DecodedCapture::default()));
    let observer = DecodedObserver(decoded.clone());
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

type DecodedChunk = (kithara::signal::AudioChunkInfo, Vec<f32>);

#[derive(Clone, Default)]
pub(super) struct DecodedCapture {
    chunks: std::collections::BTreeMap<(u64, u64), DecodedChunk>,
    samples: usize,
    rejected: bool,
}

pub(super) struct DecodedObserver(pub(super) Arc<Mutex<DecodedCapture>>);

impl kithara::audio::AudioObserver for DecodedObserver {
    fn try_observe(
        &mut self,
        chunk: &kithara::signal::AudioChunk,
    ) -> Result<(), kithara::audio::AudioObserveError> {
        let mut capture = self
            .0
            .try_lock()
            .map_err(|_| kithara::audio::AudioObserveError::Full)?;
        let samples = chunk.as_ref();
        if samples.is_empty()
            || samples.len() > 32_768
            || samples.len() != chunk.frames() * usize::from(chunk.meta.spec.channels)
            || capture.chunks.len() >= 8_192
            || capture.samples + samples.len() > 16_777_216
        {
            capture.rejected = true;
            return Err(kithara::audio::AudioObserveError::Full);
        }
        let key = (chunk.meta.source_revision, chunk.meta.frame_offset);
        if let Some((previous, pcm)) = capture.chunks.get(&key) {
            if *previous != chunk.meta || pcm.as_slice() != samples {
                capture.rejected = true;
            }
        } else {
            capture.samples += samples.len();
            capture.chunks.insert(key, (chunk.meta, samples.to_vec()));
        }
        drop(capture);
        Ok(())
    }
}

impl DecodedCapture {
    fn sample(
        &self,
        info: super::consumed::SourceIdentity,
        frame: i64,
        channel: usize,
    ) -> Result<f32, String> {
        let frame = u64::try_from(frame).map_err(|error| error.to_string())?;
        let (_, (decoded, samples)) = self
            .chunks
            .range(..=(info.source_revision, frame))
            .next_back()
            .ok_or_else(|| format!("decoder observer has no PCM at frame {frame}"))?;
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

    pub(super) fn physical_failures(
        &self,
        pcm: &[f32],
        marks: &[(SessionFrame, kithara_render::bridge::SlotSnapshot)],
        consumed: &super::consumed::Capture,
        grid: &BeatGridModel,
        wrong_source: &Audio,
        jump: Option<(SessionFrame, SessionFrame)>,
    ) -> Vec<String> {
        self.check_physical(pcm, marks, consumed, grid, wrong_source, jump)
            .err()
            .into_iter()
            .collect()
    }

    pub(super) fn physical_frame_basis(
        &self,
        info: impl Into<super::consumed::SourceIdentity>,
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

    pub(super) fn physical_basis(
        &self,
        info: impl Into<super::consumed::SourceIdentity>,
        position: (u128, std::num::NonZeroU128),
    ) -> Result<(u64, kithara::signal::SourceSpan), String> {
        let info = info.into();
        for (decoded, _) in self.chunks.values().filter(|(decoded, _)| {
            decoded.source_revision == info.source_revision
                && decoded.spec == info.spec
                && decoded.media_origin == info.media_origin
        }) {
            let basis = decoded
                .source_geometry()
                .ok_or_else(|| "decoded PCM has no source geometry".to_owned())?;
            let basis = basis
                .scaled_from(
                    (0, std::num::NonZeroU128::MIN),
                    (0, std::num::NonZeroU128::MIN),
                    (
                        u128::from(info.spec.sample_rate.get()),
                        std::num::NonZeroU128::from(basis.sample_rate()),
                    ),
                )
                .ok_or_else(|| "decoded PCM source clock cannot be normalized".to_owned())?;
            let Some((frame, denominator)) =
                basis.output_ratio_at_source(decoded.frame_offset, position)
            else {
                continue;
            };
            let frame =
                u64::try_from(frame / denominator.get()).map_err(|error| error.to_string())?;
            if decoded.frame_range().contains(&frame) {
                return Ok((decoded.frame_offset, basis));
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

    pub(super) fn sample_at(
        &self,
        info: super::consumed::SourceIdentity,
        position: (u128, std::num::NonZeroU128),
        speed: f64,
        channel: usize,
    ) -> Result<f32, String> {
        if self.rejected {
            return Err("decoder observer rejected or conflicted while capturing PCM".to_owned());
        }
        let source =
            i64::try_from(position.0 / position.1.get()).map_err(|error| error.to_string())?;
        mapped_sample((position.0, position.1.get()), speed, |offset| {
            self.sample(
                info,
                source
                    .checked_add(offset)
                    .ok_or_else(|| "physical sample coordinate overflows".to_owned())?,
                channel,
            )
        })
    }

    pub(super) fn fixture_domain(
        &self,
        info: kithara::signal::AudioChunkInfo,
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

    pub(super) fn sample_at_domain(
        &self,
        info: super::consumed::SourceIdentity,
        position: (u128, std::num::NonZeroU128),
        speed: f64,
        channel: usize,
        domain: &std::ops::Range<u64>,
    ) -> Result<f32, String> {
        if self.rejected || domain.is_empty() {
            return Err("bounded fixture sampler has no accepted source domain".to_owned());
        }
        let first = i64::try_from(domain.start).map_err(|error| error.to_string())?;
        let last = i64::try_from(domain.end - 1).map_err(|error| error.to_string())?;
        let source =
            i64::try_from(position.0 / position.1.get()).map_err(|error| error.to_string())?;
        mapped_sample((position.0, position.1.get()), speed, |offset| {
            let frame = source
                .checked_add(offset)
                .ok_or_else(|| "fixture sample coordinate overflows".to_owned())?;
            self.sample(info, frame.clamp(first, last), channel)
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

    fn check_physical(
        &self,
        pcm: &[f32],
        marks: &[(SessionFrame, kithara_render::bridge::SlotSnapshot)],
        consumed: &super::consumed::Capture,
        grid: &BeatGridModel,
        wrong_source: &Audio,
        jump: Option<(SessionFrame, SessionFrame)>,
    ) -> Result<(), String> {
        let (capture_start, frames) = self.physical_capture_clock(pcm, marks)?;
        let sample_rate = consumed.sample_rate;
        check_physical_marks(marks, sample_rate)?;
        let capture_end = i64::from(capture_start)
            .checked_add(i64::try_from(frames).map_err(|error| error.to_string())?)
            .ok_or_else(|| "physical capture endpoint overflows".to_owned())?;
        let intervals = consumed.covered(i64::from(capture_start)..capture_end)?;
        if let Some((_, landing)) = jump
            && !intervals.contains_key(&i64::from(landing))
        {
            return Err("commanded Jump landing has no actual source interval".to_owned());
        }
        let physical_bases = intervals
            .iter()
            .map(|(&start, interval)| {
                let info = interval.identity;
                let position = interval
                    .source
                    .source_ratio_at(0)
                    .ok_or_else(|| "physical source interval has no raw origin".to_owned())?;
                let (origin, basis) = self.physical_basis(info, position)?;
                let first = basis
                    .output_ratio_at_source(origin, position)
                    .ok_or_else(|| "wrong-tempo negative has no physical origin".to_owned())?;
                Ok((start, (origin, basis, first)))
            })
            .collect::<Result<std::collections::BTreeMap<_, _>, String>>()?;
        let mut expected = Vec::with_capacity(pcm.len());
        let mut wrong_cue = Vec::with_capacity(pcm.len());
        let mut wrong_tempo = Vec::with_capacity(pcm.len());
        let mut wrong_payload = Vec::with_capacity(pcm.len());
        let mut supplied_beats = Vec::new();
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
            let raw_position = span
                .source_ratio_at(offset)
                .ok_or_else(|| "physical capture has an unmapped source frame".to_owned())?;
            let raw_next = span
                .source_ratio_at(offset + 1)
                .ok_or_else(|| "physical capture has an unmapped source endpoint".to_owned())?;
            let &(origin, basis, first) = physical_bases
                .get(&start)
                .ok_or_else(|| "physical source interval lost its decoded basis".to_owned())?;
            let (numerator, denominator) = basis
                .output_ratio_at_source(origin, raw_position)
                .ok_or_else(|| "raw source coordinate has no physical PCM position".to_owned())?;
            let physical_next = basis
                .output_ratio_at_source(origin, raw_next)
                .ok_or_else(|| "raw source endpoint has no physical PCM position".to_owned())?;
            let speed = physical_source_speed((numerator, denominator), physical_next)?;
            for channel in 0..usize::from(CHANNELS) {
                let sample = self.sample_at(info, (numerator, denominator), speed, channel)?
                    * gain
                    * CONTROL_GAIN;
                if !sample.is_finite() || sample.abs() >= LimiterConfig::default().ceiling() {
                    return Err(format!(
                        "physical reference has no limiter headroom at frame {session}"
                    ));
                }
                expected.push(sample);
                let wrong_position = numerator
                    .checked_add(denominator.get())
                    .ok_or_else(|| "wrong-cue negative coordinate overflows".to_owned())?;
                let wrong_frame = i64::try_from(wrong_position / denominator.get())
                    .map_err(|error| error.to_string())?;
                wrong_cue.push(
                    mapped_sample((wrong_position, denominator.get()), speed, |offset| {
                        self.sample(
                            info,
                            wrong_frame
                                .checked_add(offset)
                                .ok_or_else(|| "wrong-cue negative overflows".to_owned())?,
                            channel,
                        )
                    })? * gain
                        * CONTROL_GAIN,
                );
                let wrong_denominator = denominator
                    .get()
                    .checked_mul(first.1.get())
                    .and_then(|value| value.checked_mul(2))
                    .ok_or_else(|| "wrong-tempo negative denominator overflows".to_owned())?;
                let wrong_numerator = numerator
                    .checked_mul(first.1.get())
                    .and_then(|value| value.checked_add(first.0.checked_mul(denominator.get())?))
                    .ok_or_else(|| "wrong-tempo negative coordinate overflows".to_owned())?;
                let wrong_frame = i64::try_from(wrong_numerator / wrong_denominator)
                    .map_err(|error| error.to_string())?;
                wrong_tempo.push(
                    mapped_sample(
                        (wrong_numerator, wrong_denominator),
                        speed * 0.5,
                        |offset| {
                            self.sample(
                                info,
                                wrong_frame
                                    .checked_add(offset)
                                    .ok_or_else(|| "wrong-tempo negative overflows".to_owned())?,
                                channel,
                            )
                        },
                    )? * gain
                        * CONTROL_GAIN,
                );
                let wrong_frame = (frame as u128 * u128::from(wrong_source.sample_rate))
                    / u128::from(sample_rate);
                wrong_payload.push(
                    source_sample(wrong_source, (wrong_frame, 1), 1.0, channel)?
                        * gain
                        * CONTROL_GAIN,
                );
            }
            physical_supplied_beats(
                &mut supplied_beats,
                info,
                raw_position,
                raw_next,
                grid,
                sample_rate,
                frame,
            )?;
        }
        PhysicalReference {
            expected,
            wrong_cue,
            wrong_tempo,
            wrong_payload,
            supplied_beats,
        }
        .check(pcm, sample_rate, frames)
    }
}

fn physical_supplied_beats(
    supplied_beats: &mut Vec<(i64, usize)>,
    info: super::consumed::SourceIdentity,
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

pub(super) fn physical_source_speed(
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
    fn check(self, pcm: &[f32], sample_rate: u32, frames: usize) -> Result<(), String> {
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
        Ok(())
    }
}
