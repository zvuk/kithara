# CI bootstrap

Minimal Rust entry point for compiling xtask under the provisioned compiler
cache and holding the CI target lease. The full xtask reuses its cache policy
and runtime lifecycle. Dependencies come from the workspace.

The driver's own build uses the runner's provisioned kache host layer. The
compiled driver validates an isolated writable RustFS daemon before starting
the full xtask build. Target leasing preserves the caller's CI environment.
