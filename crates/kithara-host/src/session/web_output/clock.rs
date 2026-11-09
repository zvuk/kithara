use bevy_platform::time::Instant;
use triple_buffer::Input;

use super::{device::OutputLease, worklet::ClockStamp};

pub(crate) struct WebClock {
    pub(super) lease: OutputLease,
    pub(super) input: Input<ClockStamp>,
    pub(super) requested: u32,
    pub(super) acknowledged: bool,
    pub(super) info: firewheel::ActivateInfo,
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
    pub(crate) fn requested(&self) -> u32 {
        self.requested
    }
    pub(crate) fn acknowledge(&mut self) {
        self.acknowledged = true;
    }
    pub(crate) fn info(&self) -> firewheel::ActivateInfo {
        self.info
    }
    pub(crate) fn initial(&self) -> bool {
        self.initial
    }
}
