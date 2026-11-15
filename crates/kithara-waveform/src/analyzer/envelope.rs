use std::num::NonZeroUsize;

use kithara_bufpool::{HasPool, PoolRegion, SampleBuffer};
use kithara_dsp::filter::{Biquad, Coefficients, FilterError, Hertz, Type};
use num_traits::ToPrimitive;

use super::waveform::Columns;
use crate::{AnalysisParams, AnalyzerError, consts};

pub(super) struct Envelope {
    first: Biquad,
    second: Biquad,
    first_planes: Vec<SampleBuffer>,
    second_planes: Vec<SampleBuffer>,
    gain: [f32; 4],
    column_frames: usize,
}

impl Envelope {
    pub(super) fn new<S>(
        rate: u32,
        params: AnalysisParams,
        column_frames: usize,
        pools: &PoolRegion<S>,
    ) -> Result<Self, AnalyzerError>
    where
        S: HasPool<f32>,
    {
        let edges = params.band_edges_hz();
        let gain = params.band_gain();
        if edges.iter().any(|edge| !edge.is_finite() || *edge <= 0.0)
            || edges.windows(2).any(|pair| pair[0] >= pair[1])
            || gain.iter().any(|gain| !gain.is_finite() || *gain < 0.0)
        {
            return Err(FilterError::Parameters.into());
        }
        let two = NonZeroUsize::MIN.saturating_add(1);
        let mut first = Biquad::new(NonZeroUsize::MIN.saturating_add(7), two)?;
        let mut second = Biquad::new(NonZeroUsize::MIN.saturating_add(3), two)?;
        for (plane, (kind, cutoff)) in [
            (Type::LowPass, edges[0]),
            (Type::HighPass, edges[0]),
            (Type::HighPass, edges[1]),
            (Type::HighPass, edges[2]),
        ]
        .into_iter()
        .enumerate()
        {
            let coefficients = design(kind, cutoff, rate)?;
            lr4(&mut first, plane, coefficients)?;
            lr4(&mut first, plane + 4, coefficients)?;
        }
        for plane in 0..4 {
            let coefficients = design(Type::LowPass, edges[plane % 2 + 1], rate)?;
            lr4(&mut second, plane, coefficients)?;
        }

        let frames = column_frames * consts::WINDOW_COLUMNS;
        let planes = |count| {
            (0..count)
                .map(|_| pools.get_with_len::<f32>(frames))
                .collect::<Result<Vec<_>, _>>()
        };
        Ok(Self {
            first,
            second,
            first_planes: planes(8)?,
            second_planes: planes(4)?,
            gain,
            column_frames,
        })
    }

    pub(super) fn reduce(
        &mut self,
        samples: &[f32],
        first: bool,
        valid_frames: usize,
    ) -> Result<Columns, AnalyzerError> {
        let frames = self.column_frames * consts::WINDOW_COLUMNS;
        for (plane, target) in self.first_planes.iter_mut().enumerate() {
            for (sample, frame) in target
                .iter_mut()
                .zip(samples.as_chunks::<2>().0)
                .take(valid_frames)
            {
                *sample = frame[plane / 4];
            }
            target[valid_frames..].fill(0.0);
        }
        let first_planes: &mut [SampleBuffer; 8] = self
            .first_planes
            .as_mut_slice()
            .try_into()
            .expect("eight prepared planes");
        self.first.process_window(
            &mut first_planes.each_mut().map(|plane| &mut plane[..]),
            0..frames,
        )?;
        for (target, source) in self.second_planes.iter_mut().zip([1, 2, 5, 6]) {
            target.copy_from_slice(&self.first_planes[source]);
        }
        let second: &mut [SampleBuffer; 4] = self
            .second_planes
            .as_mut_slice()
            .try_into()
            .expect("four prepared planes");
        self.second.process_window(
            &mut second.each_mut().map(|plane| &mut plane[..]),
            0..frames,
        )?;
        let mut columns = [[0; 8]; consts::COLUMNS];
        let offset = if first { 0 } else { frames / 4 };
        for (lane, output) in [0, 1, 2, 3, 4, 5, 6, 7].into_iter().enumerate() {
            let band = lane % 4;
            let source = match band {
                1 | 2 => &self.second_planes[lane / 4 * 2 + band - 1],
                _ => &self.first_planes[lane],
            };
            for (column, chunk) in columns
                .iter_mut()
                .zip(source[offset.min(valid_frames)..valid_frames].chunks(self.column_frames))
            {
                let peak = chunk
                    .iter()
                    .fold(0.0_f32, |peak, sample| peak.max(sample.abs()));
                column[output] = (255.0 * (self.gain[band] * peak).min(1.0).sqrt())
                    .round()
                    .to_u8()
                    .unwrap_or(255);
            }
        }
        Ok(columns)
    }
}

fn lr4(filter: &mut Biquad, plane: usize, design: Coefficients<f64>) -> Result<(), FilterError> {
    filter.retune_plane(plane, 0, design)?;
    filter.retune_plane(plane, 1, design)
}

fn design(kind: Type<f64>, cutoff: f64, rate: u32) -> Result<Coefficients<f64>, FilterError> {
    if cutoff >= f64::from(rate) * 0.5 {
        let b0 = if matches!(kind, Type::LowPass) {
            1.0
        } else {
            0.0
        };
        return Ok(Coefficients {
            b0,
            b1: 0.0,
            b2: 0.0,
            a1: 0.0,
            a2: 0.0,
        });
    }
    Coefficients::from_params(
        kind,
        Hertz::from_hz(f64::from(rate)).map_err(|_| FilterError::Parameters)?,
        Hertz::from_hz(cutoff).map_err(|_| FilterError::Parameters)?,
        std::f64::consts::FRAC_1_SQRT_2,
    )
    .map_err(|_| FilterError::Parameters)
}
