#[derive(Clone, Debug)]
pub(crate) struct Ewma {
    alpha: f64,
    last_estimate: f64,
    total_weight: f64,
}

const HALF_LIFE_BASE: f64 = 0.5;
const MIN_HALF_LIFE_SECS: f64 = 0.001;
const MIN_ZERO_FACTOR: f64 = 1e-6;

impl Ewma {
    pub(crate) fn new(half_life_secs: f64) -> Self {
        Self {
            alpha: f64::exp(HALF_LIFE_BASE.ln() / half_life_secs.max(MIN_HALF_LIFE_SECS)),
            last_estimate: 0.0,
            total_weight: 0.0,
        }
    }

    pub(crate) fn add_sample(&mut self, weight: f64, val: f64) {
        let adj_alpha = self.alpha.powf(weight.max(0.0));
        self.last_estimate = val * (1.0 - adj_alpha) + adj_alpha * self.last_estimate;
        self.total_weight += weight.max(0.0);
    }

    pub(crate) fn get_estimate(&self) -> f64 {
        if self.total_weight <= 0.0 {
            0.0
        } else {
            let zero_factor = 1.0 - self.alpha.powf(self.total_weight);
            self.last_estimate / zero_factor.max(MIN_ZERO_FACTOR)
        }
    }
}
