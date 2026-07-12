#![forbid(unsafe_code)]

#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum BusEvent {
    Overflow { scope: u64, dropped: u64 },
}
