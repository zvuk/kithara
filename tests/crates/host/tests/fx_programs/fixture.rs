use super::*;

pub(super) const SAMPLE_RATE: u32 = 44_100;
pub(super) const CHANNELS: usize = 2;
/// Frames a deck renders before its program is due.
pub(super) const WARM: usize = 4096;
/// Frames into the capture the program's first knot lands on.
pub(super) const FIRST: usize = 37;
/// Frames the dry/wet program moves over.
pub(super) const SEGMENT: usize = 128;
/// Frames each take captures from the program's call.
pub(super) const CAPTURE: usize = 1024;
/// Frames a long program runs, longer than any take.
pub(super) const LONG: i64 = 1 << 20;
/// The constant every take plays.
pub(super) const INPUT: f32 = 0.5;

/// The wet signal of the test effect: half its input.
pub(super) fn halve(sample: f32) -> f32 {
    sample * 0.5
}

/// A memoryless effect with a linear, a logarithmic and a stepped parameter:
/// its wet signal is its input times the first two.
struct Tone {
    parameters: [FxParamDescriptor; 3],
    gain: f64,
    scale: f64,
}

impl Tone {
    fn new() -> Self {
        let domain = |range, mapping| {
            FxParamDescriptor::checked(range, FxParamUnit::Ratio, mapping)
                .expect("a finite test domain")
        };
        Self {
            parameters: [
                domain(0.0..=2.0, Some(FxParamMapping::Linear)),
                domain(1.0..=16.0, Some(FxParamMapping::Log)),
                domain(0.0..=3.0, None),
            ],
            gain: 1.0,
            scale: 1.0,
        }
    }
}

impl DeckEffect for Tone {
    fn process(&mut self, input: [&[f32]; 2], wet: [&mut [f32]; 2]) {
        for (input, wet) in input.into_iter().zip(wet) {
            for (sample, out) in input.iter().zip(wet) {
                *out = (f64::from(*sample) * self.gain * self.scale)
                    .to_f32()
                    .expect("a bounded wet sample fits f32");
            }
        }
    }

    fn parameters(&self) -> &[FxParamDescriptor] {
        &self.parameters
    }

    fn set(&mut self, change: FxParamChange) {
        match change.index {
            0 => self.gain = change.value,
            1 => self.scale = change.value,
            _ => {}
        }
    }

    fn reset(&mut self) {}

    fn tail_exhausted(&self) -> bool {
        true
    }

    fn update_sample_rate(&mut self, _: NonZeroU32) {}
}

pub(super) fn halving() -> Box<dyn DeckEffect> {
    Box::new(Curve(halve))
}

pub(super) fn tone() -> Box<dyn DeckEffect> {
    Box::new(Tone::new())
}

/// A deck of two FX units playing a constant `INPUT`, called back `callback`
/// frames at a time, with the halving effect installed on each unit, powered
/// on and fully dry.
pub(super) async fn prepared(callback: usize) -> OfflinePlayer {
    prepared_with(callback, None, halving, FxMix::MIN).await
}

/// The same deck with `effect` on each unit at `mix`, and room for
/// `parameters` effect parameters in each unit, the mixer default when unset.
/// The effect leaves the constant as it is until a program moves it.
pub(super) async fn prepared_with(
    callback: usize,
    parameters: Option<usize>,
    effect: fn() -> Box<dyn DeckEffect>,
    mix: FxMix,
) -> OfflinePlayer {
    let block = NonZeroU32::new(u32::try_from(callback).expect("callback fits u32"))
        .expect("nonzero callback");
    let harness = OfflinePlayer::with_sample_rate(
        OfflinePlayerOptions::builder()
            .block_on_underrun(true)
            .output_block_frames(block)
            .warp(
                WarpConfig::builder()
                    .render_quantum_frames(NonZeroUsize::new(callback).expect("nonzero quantum"))
                    .build(),
            )
            .fx_units(2)
            .maybe_fx_parameters(parameters)
            .build(),
        SAMPLE_RATE,
    )
    .await;
    let mut changes = Vec::new();
    for unit in 0..2 {
        changes.extend([
            DeckFxChange::Install {
                unit,
                effect: effect(),
            },
            DeckFxChange::Mix { unit, value: mix },
            DeckFxChange::Power { unit, on: true },
        ]);
    }
    harness
        .with_queue(move |queue| queue.fx(When::Next, changes))
        .await
        .expect("the deck takes its units");
    let harness = load_source(
        harness,
        asset_source(&assets::fixed_constant_wav_cd_half_44100()).into(),
    )
    .await;
    let warm = render(&harness, WARM, callback).await;
    assert!(
        warm[warm.len() - 128..].iter().all(|sample| {
            if mix == FxMix::MIN {
                sample.to_bits() == INPUT.to_bits()
            } else {
                (sample - INPUT).abs() <= f32::EPSILON
            }
        }),
        "the units leave the constant"
    );
    harness
}

pub(super) async fn render(harness: &OfflinePlayer, frames: usize, callback: usize) -> Vec<f32> {
    let mut output = Vec::with_capacity(frames * CHANNELS);
    for start in (0..frames).step_by(callback) {
        output.extend(harness.render(callback.min(frames - start)).await);
    }
    output
}

pub(super) fn frame(at: u64) -> SessionFrame {
    SessionFrame::new(i64::try_from(at).expect("a test frame fits the clock"))
}

pub(super) fn after(at: SessionFrame, frames: i64) -> SessionFrame {
    SessionFrame::new(i64::from(at).saturating_add(frames))
}

pub(super) fn layout(harness: &OfflinePlayer, unit: usize) -> LayoutRevision {
    harness.deck_snapshot().fx[unit]
        .layout
        .expect("the deck reports each unit's revision")
}

pub(super) fn fx(unit: usize, layout: LayoutRevision, target: FxTarget) -> DeckParameter {
    DeckParameter::Fx {
        unit,
        layout,
        target,
    }
}

pub(super) fn mix(unit: usize, layout: LayoutRevision) -> DeckParameter {
    fx(unit, layout, FxTarget::Mix)
}

/// A program moving `parameter` linearly from `from` at `start` to `to`
/// [`LONG`] frames later.
pub(super) fn long(
    parameter: DeckParameter,
    start: SessionFrame,
    [from, to]: [f64; 2],
) -> DeckProgram {
    DeckProgram::new(
        parameter,
        vec![
            ParameterKnot::new(start, from, CurveShape::LINEAR).expect("first knot"),
            ParameterKnot::new(after(start, LONG), to, CurveShape::HOLD).expect("last knot"),
        ],
    )
    .expect("a long program")
}
