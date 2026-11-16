use std::num::NonZeroUsize;

use kithara_bufpool::{HasPool, PoolRegion, SampleBuffer};
use kithara_dsp::filter::{Biquad, Coefficients, FilterError, Hertz, Type};

use super::waveform::Columns;
use crate::{AnalysisParams, AnalyzerError, bucket::quantize, consts};

pub(super) struct Envelope {
    filter: Biquad,
    samples: SampleBuffer,
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
        let mut filter = Biquad::new(
            NonZeroUsize::MIN.saturating_add(7),
            NonZeroUsize::MIN.saturating_add(3),
        )?;
        for (planes, kind, cutoff) in [
            ([0, 2], Type::LowPass, edges[0]),
            ([1, 3], Type::HighPass, edges[2]),
            ([4, 6], Type::HighPass, edges[0]),
            ([5, 7], Type::HighPass, edges[1]),
        ] {
            let coefficients = design(kind, cutoff, rate)?;
            for plane in planes {
                lr4(&mut filter, plane, 0, coefficients)?;
            }
        }
        for (planes, cutoff) in [([4, 6], edges[1]), ([5, 7], edges[2])] {
            let coefficients = design(Type::LowPass, cutoff, rate)?;
            for plane in planes {
                lr4(&mut filter, plane, 2, coefficients)?;
            }
        }
        let frames = column_frames * consts::WINDOW_COLUMNS;
        Ok(Self {
            filter,
            samples: pools.get_with_len::<f32>(frames * 8)?,
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
        let lanes = self.samples.as_chunks_mut::<8>().0;
        for (target, frame) in lanes
            .iter_mut()
            .zip(samples.as_chunks::<2>().0)
            .take(valid_frames)
        {
            let [left, right] = *frame;
            *target = [left, left, right, right, left, left, right, right];
        }
        lanes[valid_frames..].fill([0.0; 8]);
        self.filter.process_window(&mut self.samples, 0..frames)?;
        let mut columns = [[0; 8]; consts::COLUMNS];
        let offset = if first { 0 } else { frames / 4 };
        let lanes = self.samples.as_chunks::<8>().0;
        let body = &lanes[offset.min(valid_frames)..valid_frames];
        for (column, chunk) in columns.iter_mut().zip(body.chunks(self.column_frames)) {
            let mut peaks = [0.0_f32; 8];
            for frame in chunk {
                peaks = [
                    peaks[0].max(frame[0].abs()),
                    peaks[1].max(frame[1].abs()),
                    peaks[2].max(frame[2].abs()),
                    peaks[3].max(frame[3].abs()),
                    peaks[4].max(frame[4].abs()),
                    peaks[5].max(frame[5].abs()),
                    peaks[6].max(frame[6].abs()),
                    peaks[7].max(frame[7].abs()),
                ];
            }
            let peaks = [0, 4, 5, 1, 2, 6, 7, 3].map(|lane| peaks[lane]);
            for (lane, (height, peak)) in column.iter_mut().zip(peaks).enumerate() {
                *height = quantize((self.gain[lane % 4] * peak).min(1.0).sqrt());
            }
        }
        Ok(columns)
    }
}

fn lr4(
    filter: &mut Biquad,
    plane: usize,
    section: usize,
    design: Coefficients<f64>,
) -> Result<(), FilterError> {
    filter.retune_plane(plane, section, design)?;
    filter.retune_plane(plane, section + 1, design)
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
