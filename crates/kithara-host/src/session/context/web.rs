use super::SessionGraph;

impl SessionGraph {
    pub(crate) fn install_web_clock(&mut self, clock: crate::session::web_output::WebClock) {
        self.clock = Some(clock);
    }

    pub(crate) fn initial_output(&self) -> bool {
        self.stop.is_none()
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
