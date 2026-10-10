//! Timing parity: every engine, in the geometry the product builds, lands a
//! kick at the output frame its source advance names, so two engines fed one
//! plan place every beat alike.

use std::f64::consts::TAU;

use kithara_test_utils::{bufpool::pools as default_pools, kithara};
use num_traits::ToPrimitive;

use super::with_output_source_frames;
use crate::{ElasticBackendConfig, ElasticConfig, ElasticRequest, StretchKind, build_engine};

const CHANNELS: usize = 2;
const CONTROL_QUANTUM: usize = 64;
const SAMPLE_RATE: u32 = 48_000;

mod consts {
    /// Source frames from the cue to the first kick.
    pub(super) const LEAD_FRAMES: usize = 4_800;
    /// Source frames between kicks, wider than any engine's window.
    pub(super) const KICK_SPACING: usize = 24_000;
    pub(super) const KICKS: usize = 8;
    pub(super) const KICK_FRAMES: usize = 2_400;
    /// Frames of the mean-magnitude envelope an onset is read from.
    pub(super) const ENVELOPE_FRAMES: usize = 32;
    /// An onset crosses this fraction of its kick's envelope peak.
    pub(super) const ONSET_FRACTION: f32 = 0.5;
    /// A kick onset may land 5 ms off its projected output frame.
    pub(super) const TOLERANCE_FRAMES: f64 = 240.0;
    /// Half the window an onset is searched in around its projected frame,
    /// inside the output spacing of kicks at the fastest rate.
    pub(super) const ONSET_WINDOW_FRAMES: usize = 6_000;
    /// Output frames the ramp profile spends going from its first to its last rate.
    pub(super) const RAMP_FRAMES: usize = 96_000;
    /// Output frames between the ride profile's tempo steps, off the kick grid.
    pub(super) const RIDE_STEP_FRAMES: usize = 17_000;
    /// The ride profile's rates, stepped like a Host tempo retarget.
    pub(super) const RIDE: [f64; 6] = [0.8, 1.25, 0.7, 1.5, 1.0, 1.6];
    pub(super) const MAX_FRAMES: usize = 65_536;
    pub(super) const PITCH_DROP_HZ: f64 = 100.0;
    pub(super) const BODY_HZ: f64 = 50.0;
    pub(super) const PITCH_SECONDS: f64 = 0.02;
    pub(super) const DECAY_SECONDS: f64 = 0.05;
    pub(super) const PEAK: f64 = 0.9;
}

/// A pitched-down kick: the transient every beat grid is heard by.
fn kick() -> Vec<f64> {
    let rate = f64::from(SAMPLE_RATE);
    let mut phase = 0.0;
    (0..consts::KICK_FRAMES)
        .map(|frame| {
            let seconds = frame.to_f64().expect("frame fits f64") / rate;
            let hz =
                consts::BODY_HZ + consts::PITCH_DROP_HZ * (-seconds / consts::PITCH_SECONDS).exp();
            phase += TAU * hz / rate;
            consts::PEAK * (-seconds / consts::DECAY_SECONDS).exp() * phase.sin()
        })
        .collect()
}

/// `history` silent frames, then kicks every [`consts::KICK_SPACING`]
/// frames from [`consts::LEAD_FRAMES`] past the cue, interleaved.
fn kick_train(history: usize) -> Vec<f32> {
    let kick = kick();
    let frames = history + consts::LEAD_FRAMES + (consts::KICKS + 4) * consts::KICK_SPACING;
    let mut pcm = vec![0.0; frames * CHANNELS];
    for index in 0..consts::KICKS {
        let start = history + consts::LEAD_FRAMES + index * consts::KICK_SPACING;
        for (frame, sample) in kick.iter().enumerate() {
            let sample = sample.to_f32().expect("kick sample fits f32");
            pcm[(start + frame) * CHANNELS..(start + frame + 1) * CHANNELS].fill(sample);
        }
    }
    pcm
}

/// The first frame of each kick window whose envelope crosses
/// [`consts::ONSET_FRACTION`] of that window's peak.
fn onsets(interleaved: &[f32], expected: &[f64]) -> Vec<Option<f64>> {
    let mono: Vec<f32> = interleaved
        .chunks_exact(CHANNELS)
        .map(|frame| frame.iter().map(|sample| sample.abs()).sum::<f32>())
        .collect();
    let width = consts::ENVELOPE_FRAMES.to_f32().expect("width fits f32");
    let envelope: Vec<f32> = mono
        .windows(consts::ENVELOPE_FRAMES)
        .map(|window| window.iter().sum::<f32>() / width)
        .collect();
    let half = consts::ONSET_WINDOW_FRAMES;
    expected
        .iter()
        .map(|&at| {
            let at = at.round().to_usize()?;
            let start = at.saturating_sub(half);
            let window = envelope.get(start..at + half)?;
            let peak = window.iter().copied().fold(0.0_f32, f32::max);
            let onset = window
                .iter()
                .position(|&level| level >= peak * consts::ONSET_FRACTION)?;
            (start + onset).to_f64()
        })
        .collect()
}

/// A rate trajectory: source frames per output frame at an output frame.
type Profile = fn(usize) -> f64;

/// The rate trajectories every engine must place kicks alike on: constant
/// rates across the envelope, a steady ramp, and the step changes a
/// retargeting Host tempo makes.
fn profiles() -> [(&'static str, Profile); 7] {
    [
        ("constant 0.6", |_| 0.6),
        ("constant 0.8", |_| 0.8),
        ("constant 1.0", |_| 1.0),
        ("constant 1.25", |_| 1.25),
        ("constant 1.6", |_| 1.6),
        ("ramp 0.6 to 1.6", |output| {
            let progress = output.to_f64().expect("frame fits f64")
                / consts::RAMP_FRAMES.to_f64().expect("ramp fits f64");
            0.6 + progress.min(1.0)
        }),
        ("ride", |output| {
            consts::RIDE[(output / consts::RIDE_STEP_FRAMES).min(consts::RIDE.len() - 1)]
        }),
    ]
}

/// Source frames the first `output` output frames of `profile` advance,
/// extending the per-frame running sum in `advance` as far as asked.
fn advance_at(advance: &mut Vec<f64>, profile: Profile, output: usize) -> f64 {
    while advance.len() <= output {
        let frame = advance.len() - 1;
        advance.push(advance[frame] + profile(frame));
    }
    advance[output]
}

fn whole(frames: f64) -> usize {
    frames.floor().to_usize().expect("advance fits usize")
}

/// Kick onsets of `backend`'s output along `profile`, fed the way Warp feeds
/// a projected engine: primed at the cue, then advanced in
/// [`CONTROL_QUANTUM`]-frame requests that admit the source the projection
/// names one engine latency ahead while naming the audible advance, against
/// the output frames the audible advance projects the source onsets to.
fn misplaced_kicks(backend: StretchKind, name: &str, profile: Profile) -> Vec<String> {
    let config = ElasticConfig::builder()
        .backend(backend)
        .backends(ElasticBackendConfig::default())
        .pools(default_pools())
        .sample_rate(SAMPLE_RATE)
        .channels(CHANNELS)
        .max_source_frames(consts::MAX_FRAMES)
        .max_output_frames(consts::MAX_FRAMES)
        .build()
        .expect("the product geometry is valid");
    let mut engine = build_engine(config).expect("the product engine prepares");
    let capabilities = engine.capabilities();
    let latency = capabilities.latency();
    let history = latency.first();
    let lead = latency.second();
    let train = kick_train(history);
    let frames = train.len() / CHANNELS;
    let mut advance = vec![0.0];
    // The admitted source runs one output latency ahead of the audible source.
    let input_start = |advance: &mut Vec<f64>, output: usize| {
        history * 2 + whole(advance_at(advance, profile, output + lead))
    };
    let mut cursor = input_start(&mut advance, 0);
    // A zero-latency engine has nothing to warm, so the product never primes it.
    if lead > 0 {
        let warmup = ElasticRequest::new(cursor - history * 2, lead).expect("warmup request");
        let mut discarded = vec![0.0; lead * CHANNELS];
        engine
            .prime(
                warmup,
                &train[..history * CHANNELS],
                &train[history * CHANNELS..history * 2 * CHANNELS],
                &train[history * 2 * CHANNELS..cursor * CHANNELS],
                &mut discarded,
            )
            .expect("the product engine primes");
    }
    let cue = history.to_f64().expect("cue fits f64");
    let source_onsets: Vec<f64> = (0..consts::KICKS)
        .map(|index| {
            (history + consts::LEAD_FRAMES + index * consts::KICK_SPACING)
                .to_f64()
                .expect("frame fits f64")
        })
        .collect();
    let source_onsets: Vec<f64> = onsets(&train, &source_onsets)
        .into_iter()
        .map(|onset| onset.expect("every source kick has an onset") - cue)
        .collect();
    let needed = source_onsets.last().copied().unwrap_or_default()
        + consts::KICK_SPACING.to_f64().expect("spacing fits f64") / 2.0;
    let mut output = Vec::new();
    let mut rendered = 0;
    while advance_at(&mut advance, profile, rendered) < needed {
        let rate = profile(rendered);
        assert!(
            capabilities.rate_envelope().contains_rate(rate),
            "{backend:?} covers rate {rate}"
        );
        let end = input_start(&mut advance, rendered + CONTROL_QUANTUM);
        assert!(end <= frames, "the train covers the render");
        let audible = whole(advance_at(
            &mut advance,
            profile,
            rendered + CONTROL_QUANTUM,
        )) - whole(advance_at(&mut advance, profile, rendered));
        let request = ElasticRequest::new(end - cursor, CONTROL_QUANTUM)
            .and_then(|request| with_output_source_frames(request, audible))
            .expect("control request");
        let mut block = vec![0.0; CONTROL_QUANTUM * CHANNELS];
        engine
            .process(
                request,
                &train[cursor * CHANNELS..end * CHANNELS],
                &mut block,
            )
            .unwrap_or_else(|error| panic!("{backend:?} on {name}: {error:?}"));
        output.extend_from_slice(&block);
        cursor = end;
        rendered += CONTROL_QUANTUM;
    }
    let expected: Vec<f64> = source_onsets
        .iter()
        .map(|&onset| {
            let after = advance
                .iter()
                .position(|&advance| advance >= onset)
                .expect("the render passes every kick");
            let (from, to) = (advance[after - 1], advance[after]);
            (after - 1).to_f64().expect("frame fits f64") + (onset - from) / (to - from)
        })
        .collect();
    onsets(&output, &expected)
        .into_iter()
        .zip(&expected)
        .enumerate()
        .filter_map(|(index, (onset, &at))| match onset {
            Some(onset) if (onset - at).abs() <= consts::TOLERANCE_FRAMES => None,
            Some(onset) => Some(format!(
                "{name}: kick {index} onset {:+.0} frames off output frame {at:.0}",
                onset - at
            )),
            None => Some(format!("{name}: kick {index} has no onset near {at:.0}")),
        })
        .collect()
}

#[kithara::test]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith)
)]
#[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
#[cfg_attr(feature = "stretch-glide", case::glide(StretchKind::Glide))]
fn product_engines_land_kicks_where_the_source_advance_projects_them(#[case] backend: StretchKind) {
    let misplaced: Vec<String> = profiles()
        .into_iter()
        .flat_map(|(name, profile)| misplaced_kicks(backend, name, profile))
        .collect();
    assert!(
        misplaced.is_empty(),
        "{backend:?} misplaced kicks:\n{}",
        misplaced.join("\n")
    );
}
