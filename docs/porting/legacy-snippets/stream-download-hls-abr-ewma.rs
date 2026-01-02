/// Exponentially-weighted moving average (EWMA).
///
/// Maintains a weighted moving average that emphasizes recent samples.
/// Useful for estimating bandwidth in streaming scenarios, where newer measurements should have
/// more influence than older ones.
#[derive(Debug)]
pub(crate) struct Ewma {
    alpha: f64,
    last_estimate: f64,
    total_weight: f64,
}

impl Ewma {
    /// Creates a new EWMA with the given half-life (in seconds).
    pub(crate) fn new(half_life: u32) -> Self {
        Self {
            alpha: f64::exp(0.5f64.ln() / f64::from(half_life)),
            last_estimate: 0.,
            total_weight: 0.,
        }
    }

    /// Adds a new sample.
    ///
    /// `val` is the observed value and `weight` controls how much time/importance this sample
    /// represents (for example, duration in seconds).
    pub(crate) fn add_sample(&mut self, weight: f64, val: f64) {
        let adj_alpha = self.alpha.powf(weight);
        let new_estimate = val * (1. - adj_alpha) + adj_alpha * self.last_estimate;
        self.last_estimate = new_estimate;
        self.total_weight += weight;
    }

    /// Returns the current estimate produced by the `Ewma`.
    ///
    /// Returns `0.` if it cannot produce an estimate yet.
    pub(crate) fn get_estimate(&self) -> f64 {
        if self.total_weight == 0. {
            0.
        } else {
            let zero_factor = 1. - self.alpha.powf(self.total_weight);
            self.last_estimate / zero_factor
        }
    }
}
