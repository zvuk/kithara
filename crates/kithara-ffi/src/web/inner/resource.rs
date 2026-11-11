use kithara::{abr::AbrMode, platform::sync::Arc};

use super::WasmInner;
use crate::{
    observer::FfiKeyProcessor,
    types::{FfiAbrMode, FfiKeyRule, peak_bitrate_cap},
};

impl WasmInner {
    pub(crate) fn set_abr_mode(&self, mode: FfiAbrMode) {
        let Some(handle) = self
            .bridge
            .queue()
            .and_then(|queue| queue.current_abr_handle())
        else {
            return;
        };
        if let Err(error) = handle.set_mode(AbrMode::from(mode)) {
            tracing::warn!(?error, "set_abr_mode rejected by ABR state");
        }
    }

    pub(crate) fn setup_hls_aes(&self, processor: Arc<dyn FfiKeyProcessor>) {
        self.setup_hls_aes_with_rule(FfiKeyRule {
            processor,
            headers: None,
            query_params: None,
            domains: vec!["*".to_owned()],
            salt: Some(crate::web::interop::generate_salt()),
        });
    }

    pub(crate) fn setup_hls_aes_with_rule(&self, mut rule: FfiKeyRule) {
        crate::web::keys::install_main_processor(Arc::clone(&rule.processor));
        let salt = rule
            .salt
            .take()
            .unwrap_or_else(crate::web::interop::generate_salt);
        self.report(self.bridge.register_key_rule(rule, salt));
    }

    pub(crate) fn setup_network(&self, auth_token: String) {
        self.report(self.bridge.set_auth_token(auth_token));
    }

    pub(crate) fn update_peak_bitrate(&self, wifi_bps: f64, cellular_bps: f64) {
        if let Some(handle) = self
            .bridge
            .queue()
            .and_then(|queue| queue.current_abr_handle())
        {
            handle.set_max_bandwidth_bps(peak_bitrate_cap(wifi_bps, cellular_bps));
        }
    }
}
