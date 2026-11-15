use super::{GraphError, SessionGraph};

impl SessionGraph {
    pub(crate) fn install_web_clock(&mut self, clock: crate::session::web_output::WebClock) {
        self.clock = Some(clock);
        self.output_starting = true;
    }

    pub(crate) fn initial_output(&self) -> bool {
        self.stop.is_none()
    }
    pub(crate) fn output_ready(&self) -> Result<bool, GraphError> {
        if let Some(clock) = &self.clock {
            return clock.ready().map_err(GraphError::Device);
        }
        Ok(true)
    }

    pub(crate) fn requested_output_rate(&self) -> Option<u32> {
        if let Some(clock) = &self.clock {
            return Some(clock.requested());
        }
        None
    }

    pub(crate) fn acknowledge_output(&mut self) {
        self.output_starting = false;
    }
}

impl Drop for SessionGraph {
    fn drop(&mut self) {
        self.request_deactivate();
        if let Some(context) = self.inner.take() {
            wasm_bindgen_futures::spawn_local(async move {
                while context.is_active() {
                    kithara_platform::time::sleep(crate::consts::SESSION_PUMP_INTERVAL).await;
                }
                drop(context);
            });
        }
    }
}
