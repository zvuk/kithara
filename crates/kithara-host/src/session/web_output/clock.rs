use bevy_platform::time::Instant;
use triple_buffer::Input;

use super::{device::OutputLease, worklet::ClockStamp};

#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, vis = "pub(crate)")]
pub(crate) struct WebClock {
    pub(super) lease: OutputLease,
    pub(super) input: Input<ClockStamp>,
    #[field(get(copy))]
    pub(super) requested: u32,
    pub(super) acknowledged: bool,
    #[field(get(copy))]
    pub(super) info: firewheel::ActivateInfo,
    #[field(get(copy))]
    pub(super) initial: bool,
}

impl WebClock {
    pub(crate) fn ready(&self) -> Result<bool, String> {
        match self.lease.error() {
            Some(error) => Err(error),
            None => Ok(self.lease.ready()),
        }
    }

    pub(crate) fn publish(&mut self) -> Result<(), String> {
        self.ready()?;
        self.input.write(ClockStamp {
            main: Instant::now(),
            audio: self.lease.current_time(),
        });
        Ok(())
    }

    pub(crate) fn starting(&self) -> bool {
        !self.acknowledged
    }
    pub(crate) fn acknowledge(&mut self) {
        self.acknowledged = true;
    }
}
