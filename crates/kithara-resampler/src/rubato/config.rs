use bon::Builder;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub enum RubatoAlgorithm {
    #[default]
    Async,
    Fft,
}

#[derive(Clone, Copy, Debug, Builder, Eq, PartialEq)]
#[builder(state_mod(vis = "pub"))]
#[non_exhaustive]
pub struct RubatoConfig {
    #[builder(default)]
    pub algorithm: RubatoAlgorithm,
}

impl Default for RubatoConfig {
    fn default() -> Self {
        Self {
            algorithm: RubatoAlgorithm::Async,
        }
    }
}
