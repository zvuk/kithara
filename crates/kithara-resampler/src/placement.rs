use std::fmt;

#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ResamplerPlacement {
    Standalone,
    DecoderEmbedded,
}

impl fmt::Display for ResamplerPlacement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Standalone => f.write_str("standalone"),
            Self::DecoderEmbedded => f.write_str("decoder-embedded"),
        }
    }
}
