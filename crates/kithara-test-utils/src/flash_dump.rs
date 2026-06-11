/// Dump the flash quiescence-engine state to stderr. The `#[kithara::test]`
/// harness calls this from both hang exits (virtual-timeout panic and the
/// HARD TIMEOUT abort thread) so a wedged run self-reports every parked
/// participant, deadline and pending signal instead of dying opaque.
#[cfg(all(feature = "flash", not(target_arch = "wasm32")))]
pub fn flash_dump_to_stderr(context: &str) {
    eprintln!(
        "[flash-dump] {context}:\n{}",
        kithara_platform::time::flash_dump_state()
    );
}

/// No engine to report without the `flash` feature (or on wasm).
#[cfg(not(all(feature = "flash", not(target_arch = "wasm32"))))]
pub fn flash_dump_to_stderr(_context: &str) {}
