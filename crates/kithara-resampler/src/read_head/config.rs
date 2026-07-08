use bon::Builder;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub enum ReadHeadInterpolation {
    Linear,
    #[default]
    Quadratic,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Builder)]
#[builder(state_mod(vis = "pub"))]
#[non_exhaustive]
pub struct ReadHeadConfig {
    #[builder(default = true)]
    pub anti_alias: bool,
    #[builder(default)]
    pub interpolation: ReadHeadInterpolation,
}

impl Default for ReadHeadConfig {
    fn default() -> Self {
        Self {
            anti_alias: true,
            interpolation: ReadHeadInterpolation::Quadratic,
        }
    }
}
