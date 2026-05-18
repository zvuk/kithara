use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

pub fn setup_tracing() {
    setup_tracing_with_filter("warn");
}

pub fn setup_tracing_with_filter(directives: &str) {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(directives));
    init_tracing(filter);
}

pub fn init_tracing(filter: EnvFilter) {
    #[cfg(not(target_arch = "wasm32"))]
    {
        use tracing_subscriber::Layer as _;

        let fmt_layer = tracing_subscriber::fmt::layer()
            .with_test_writer()
            .with_filter(filter);
        let probe_layer = crate::probe::capture::probe_layer();
        let _ = tracing_subscriber::registry()
            .with(fmt_layer)
            .with(probe_layer)
            .try_init();
    }

    #[cfg(target_arch = "wasm32")]
    {
        use tracing_wasm::{WASMLayer, WASMLayerConfigBuilder};

        let mut config = WASMLayerConfigBuilder::new();
        config.set_report_logs_in_timings(false);
        let subscriber = tracing_subscriber::registry().with(filter);
        let subscriber = subscriber.with(WASMLayer::new(config.build()));
        let _ = subscriber.try_init();
    }
}
