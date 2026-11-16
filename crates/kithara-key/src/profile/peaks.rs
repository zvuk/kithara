use num_traits::cast::ToPrimitive;

use crate::consts;

pub(crate) type Peak = [f32; 2];

struct Bins<'a> {
    magnitudes: &'a [f32],
    hz_per_bin: f32,
    last: usize,
}

struct Found<'a> {
    out: &'a mut [Peak],
    len: usize,
}

pub(crate) fn detect(magnitudes: &[f32], nyquist: f32, out: &mut [Peak]) -> usize {
    let Some(last) = magnitudes.len().checked_sub(1).filter(|&last| last >= 1) else {
        return 0;
    };
    let bins = Bins {
        magnitudes,
        last,
        hz_per_bin: nyquist / bin_f32(last),
    };
    let mut found = Found { out, len: 0 };

    let mut i = bins.first();
    if bins.falls_after(i) {
        found.push([bins.hz(bin_f32(i)), bins.at(i)]);
    }
    loop {
        i = bins.climb(i);
        let j = bins.plateau_end(i);
        if let Some(peak) = bins.peak(i, j) {
            let [hz, _] = peak;
            if hz > consts::MAX_HZ {
                break;
            }
            found.push(peak);
        }
        i = j;
        if i + 1 >= last {
            if let Some(peak) = bins.last_inner_peak(i) {
                found.push(peak);
            }
            break;
        }
    }
    if let Some(peak) = bins.top_edge_peak() {
        found.push(peak);
    }
    found.strongest()
}

impl Bins<'_> {
    fn at(&self, bin: usize) -> f32 {
        self.magnitudes.get(bin).copied().unwrap_or(0.0)
    }

    fn climb(&self, mut bin: usize) -> usize {
        while bin + 1 < self.last && self.at(bin) >= self.at(bin + 1) {
            bin += 1;
        }
        while bin + 1 < self.last && self.at(bin) < self.at(bin + 1) {
            bin += 1;
        }
        bin
    }

    fn falls_after(&self, bin: usize) -> bool {
        bin < self.last && self.at(bin) > self.at(bin + 1) && loud(self.at(bin))
    }

    fn first(&self) -> usize {
        (consts::MIN_HZ / self.hz_per_bin)
            .ceil()
            .to_usize()
            .unwrap_or(0)
    }

    fn last_inner_peak(&self, bin: usize) -> Option<Peak> {
        let rises = self.at(bin.saturating_sub(1)) < self.at(bin);
        let falls = self.at(bin + 1) < self.at(bin);
        (bin + 1 == self.last && rises && falls && loud(self.at(bin))).then(|| self.vertex(bin))
    }

    fn hz(&self, bin: f32) -> f32 {
        bin * self.hz_per_bin
    }

    fn peak(&self, start: usize, end: usize) -> Option<Peak> {
        if end + 1 >= self.last || self.at(end + 1) >= self.at(end) || !loud(self.at(end)) {
            return None;
        }
        if end == start {
            return Some(self.vertex(end));
        }
        Some([self.hz(bin_f32(start + end) * 0.5), self.at(start)])
    }

    fn plateau_end(&self, mut bin: usize) -> usize {
        while bin + 1 < self.last && self.at(bin) == self.at(bin + 1) {
            bin += 1;
        }
        bin
    }

    fn top_edge_peak(&self) -> Option<Peak> {
        let limit = consts::MAX_HZ / self.hz_per_bin;
        let in_band = bin_f32(self.last - 1) < limit && limit <= bin_f32(self.last);
        let rises = self.at(self.last) > self.at(self.last - 1);
        (in_band && rises && loud(self.at(self.last)))
            .then(|| [self.hz(bin_f32(self.last)), self.at(self.last)])
    }

    fn vertex(&self, bin: usize) -> Peak {
        let left = self.at(bin.saturating_sub(1));
        let middle = self.at(bin);
        let right = self.at(bin + 1);
        let offset = 0.5 * ((left - right) / (left - 2.0 * middle + right));
        [
            self.hz(bin_f32(bin) + offset),
            middle - 0.25 * (left - right) * offset,
        ]
    }
}

impl Found<'_> {
    fn push(&mut self, peak: Peak) {
        if let Some(slot) = self.out.get_mut(self.len) {
            *slot = peak;
            self.len += 1;
        }
    }

    fn strongest(self) -> usize {
        if let Some(peaks) = self.out.get_mut(..self.len) {
            peaks.sort_unstable_by(|[a_hz, a_magnitude], [b_hz, b_magnitude]| {
                b_magnitude
                    .total_cmp(a_magnitude)
                    .then(a_hz.total_cmp(b_hz))
            });
        }
        self.len.min(consts::MAX_PEAKS)
    }
}

pub(crate) fn bin_f32(bin: usize) -> f32 {
    bin.to_f32().unwrap_or(f32::NAN)
}

fn loud(magnitude: f32) -> bool {
    magnitude > consts::PEAK_THRESHOLD
}
