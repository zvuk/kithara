bitflags::bitflags! {
    #[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
    pub struct ResamplerCapabilities: u32 {
        const FIXED_RATIO = 1 << 0;
        const VARIABLE_RATIO = 1 << 1;
        const RATIO_GLIDE = 1 << 2;
        const REALTIME_SAFE = 1 << 3;
        const REPORTS_LATENCY = 1 << 4;
        const STANDALONE = 1 << 5;
    }
}
