use tracing_subscriber::EnvFilter;

pub fn setup_tracing() {
    setup_tracing_with_filter("warn");
}

pub fn setup_tracing_with_filter(directives: &str) {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(directives));
    init_tracing(filter);
}

pub fn init_tracing(filter: EnvFilter) {
    // RealtimeSanitizer lane (`--cfg rtsan`, the native sanitizer build only):
    // a capturing/formatting subscriber allocates on the audio worker thread —
    // the forbid-blocking produce core — whenever it captures, or merely
    // *constructs* (recording `?`/`%` Debug fields keeps the callsite live via
    // the probe layer), a decode/seek diagnostic event. Production RT threads
    // are never synchronously logged, so install no subscriber here: callsites
    // stay disabled and the lane verifies kithara's own RT-safety, not the test
    // logger. Normal `just test` keeps the full fmt + probe subscriber.
    #[cfg(rtsan)]
    {
        let _ = filter;
    }

    #[cfg(all(not(target_arch = "wasm32"), not(rtsan)))]
    {
        use tracing_subscriber::{Layer as _, layer::SubscriberExt, util::SubscriberInitExt};

        let fmt_layer = tracing_subscriber::fmt::layer()
            .with_test_writer()
            .with_filter(filter);
        let probe_layer = crate::probe::capture::probe_layer();
        let _ = tracing_subscriber::registry()
            .with(fmt_layer)
            .with(probe_layer)
            .try_init();
    }

    #[cfg(all(target_arch = "wasm32", not(rtsan)))]
    {
        use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
        use tracing_wasm::{WASMLayer, WASMLayerConfigBuilder};

        let mut config = WASMLayerConfigBuilder::new();
        config.set_report_logs_in_timings(false);
        let subscriber = tracing_subscriber::registry().with(filter);
        let subscriber = subscriber.with(WASMLayer::new(config.build()));
        let _ = subscriber.try_init();
    }
}
