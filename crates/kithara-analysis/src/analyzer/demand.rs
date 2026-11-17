use bitflags::bitflags;

bitflags! {
    /// Which artifacts one pass is opened to produce.
    ///
    /// A caller that already holds a prepared artifact asks for the rest; the
    /// analyzers a configuration enables never change, only what a single pass
    /// is opened for. A pass reports the fingerprint of what it actually
    /// produced, so a narrowed result is never mistaken for a full one.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub struct AnalysisDemand: u8 {
        /// A beat artifact is asked for.
        const BEAT = 1 << 0;
        /// A waveform is asked for.
        const WAVEFORM = 1 << 1;
        /// A key estimate is asked for.
        const KEY = 1 << 2;
    }
}

impl AnalysisDemand {
    /// Everything the configuration can produce.
    pub const ALL: Self = Self::all();

    /// Whether a beat artifact is asked for.
    #[must_use]
    pub const fn beat(self) -> bool {
        self.contains(Self::BEAT)
    }

    /// Whether a waveform is asked for.
    #[must_use]
    pub const fn waveform(self) -> bool {
        self.contains(Self::WAVEFORM)
    }

    /// Whether a key estimate is asked for.
    #[must_use]
    pub const fn key(self) -> bool {
        self.contains(Self::KEY)
    }
}

impl Default for AnalysisDemand {
    fn default() -> Self {
        Self::ALL
    }
}
