use kithara::{abr::AbrMode, platform::sync::Arc};

use super::WasmInner;
use crate::{
    observer::FfiKeyProcessor,
    types::{FfiAbrMode, FfiKeyRule},
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
        let mode = match mode {
            FfiAbrMode::Auto => AbrMode::Auto(None),
            FfiAbrMode::Manual { variant_index } => AbrMode::manual(variant_index as usize),
        };
        if let Err(error) = handle.set_mode(mode) {
            self.routes.error(error.to_string());
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
            handle.set_max_bandwidth_bps(effective_cap(wifi_bps, cellular_bps));
        }
    }
}

fn effective_cap(wifi_bps: f64, cellular_bps: f64) -> Option<u64> {
    const U64_MAX_AS_F64: f64 = 18_446_744_073_709_551_615.0;
    let cap = [wifi_bps, cellular_bps]
        .into_iter()
        .filter(|v| *v > 0.0)
        .reduce(f64::min)
        .filter(|v| v.is_finite() && *v > 0.0)?;
    if cap >= U64_MAX_AS_F64 {
        return Some(u64::MAX);
    }
    Some(num_traits::cast(cap.trunc()).unwrap_or(u64::MAX))
}
