#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
#[non_exhaustive]
pub enum PrerollHint {
    /// Scheduler must ensure this byte is fetched before seek completes.
    Required(u64),
    /// Codec does not need priming (FLAC, PCM, Symphonia AAC, etc.)
    #[default]
    NotNeeded,
    /// First segment / no prior data exists to prime from.
    FirstSegment,
}
