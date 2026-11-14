use firewheel::error::DeactivateError;

use super::SessionGraph;

impl SessionGraph {
    pub(crate) fn deactivate_blocking(
        &mut self,
        timeout: kithara_platform::time::Duration,
    ) -> Result<(), DeactivateError> {
        let until = kithara_platform::time::Instant::now() + timeout;
        self.request_deactivate();
        while self.is_active() {
            if kithara_platform::time::Instant::now() >= until {
                return Err(DeactivateError::TimedOut);
            }
            std::thread::yield_now();
        }
        Ok(())
    }
}

impl Drop for SessionGraph {
    fn drop(&mut self) {
        self.request_deactivate();
    }
}
