/// Frequency order shared by each side of a waveform column.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(usize)]
pub enum Band {
    Low = 0,
    LowMid = 1,
    HighMid = 2,
    High = 3,
}

impl Band {
    pub const COUNT: usize = 4;

    #[must_use]
    pub const fn idx(self) -> usize {
        self as usize
    }
}
