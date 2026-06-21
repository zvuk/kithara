pub fn log_error(msg: &str) {
    tracing::error!(target: "kithara_platform", "{msg}");
}
